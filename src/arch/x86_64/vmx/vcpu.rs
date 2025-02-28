use alloc::collections::VecDeque;
use core::fmt::{Debug, Formatter, Result};
use core::{arch::asm, mem::size_of};

use bit_field::BitField;
use x86::bits64::vmx;
use x86::dtables::{self, DescriptorTablePointer};
use x86::segmentation::SegmentSelector;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr3, Cr4, Cr4Flags};

use super::region::{MsrBitmap, VmxRegion};
use super::vmcs::{
    self, VmcsControl32, VmcsControl64, VmcsControlNW, VmcsGuest16, VmcsGuest32, VmcsGuest64,
    VmcsGuestNW, VmcsHost16, VmcsHost32, VmcsHost64, VmcsHostNW,
};
use super::VmxPerCpuState;
use super::definitions::VmxExitReason;
use crate::arch::{msr::Msr, memory::NestedPageFaultInfo, regs::GeneralRegisters};
use crate::arch::lapic::ApicTimer;
use crate::{GuestPhysAddr, HostPhysAddr, HyperCraftHal, HyperResult, VmExitInfo, VmxExitInfo};

static mut VMX_PREEMPTION_TIMER_SET_VALUE: u32 = 1000_000;

#[derive(Copy, Clone)]
pub enum VmCpuMode {
    Real,
    Protected,
    Compatibility, // IA-32e mode (CS.L = 0)
    Long, // IA-32E mode (CS.L = 1)
}

/// A virtual CPU within a guest.
#[repr(C)]
pub struct VmxVcpu<H: HyperCraftHal> {
    pub guest_regs: GeneralRegisters,
    pub host_stack_top: u64,
    pub vmcs: VmxRegion<H>,
    pub msr_bitmap: MsrBitmap<H>,
    pub apic_timer: ApicTimer<H>,
    pub pending_events: VecDeque<(u8, Option<u32>)>,
    pub is_launched: bool,
    pub vcpu_id: usize,
    // Information related to secondary / AP vcpu start-up
    pub cpu_mode: VmCpuMode,
    pub nr_sipi: u8,
}

impl<H: HyperCraftHal> VmxVcpu<H> {
    /// common
    pub fn new_common(vmcs_revision_id: u32, vcpu_id: usize) -> HyperResult<Self> {
        let mut vcpu = Self {
            vcpu_id,
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            vmcs: VmxRegion::new(vmcs_revision_id, false)?,
            msr_bitmap: MsrBitmap::passthrough_all()?,
            apic_timer: ApicTimer::new(),
            pending_events: VecDeque::with_capacity(8),
            is_launched: false,
            cpu_mode: VmCpuMode::Real,
            nr_sipi: 0,
        };
        vcpu.setup_msr_bitmap()?;
        Ok(vcpu)
    }

    /// Bind this [`vmxvcpu`] to current cpu
    pub fn bind_to_current_cpu(&self) -> HyperResult {
        unsafe {
            vmx::vmptrld(self.vmcs.phys_addr() as u64)?;
        }
        Ok(())
    }

    /// Unbind this [`vmxvcpu`] to current cpu
    pub fn unbind_to_current_cpu(&self) -> HyperResult {
        unsafe {
            vmx::vmclear(self.vmcs.phys_addr() as u64)?;
        }
        Ok(())
    }

    pub fn cpu_mode(&self) -> VmCpuMode {
        todo!()
    }

    /// setup start entry
    pub fn set_entry(&mut self, entry: GuestPhysAddr) -> HyperResult {
        VmcsGuestNW::RIP.write(entry)?;
        Ok(())
    }
}


impl<H: HyperCraftHal> VmxVcpu<H> {
    pub fn new(
        percpu: &VmxPerCpuState<H>,
        entry: GuestPhysAddr,
        ept_root: HostPhysAddr,
    ) -> HyperResult<Self> {
        let mut vcpu = Self {
            vcpu_id: 0,
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            vmcs: VmxRegion::new(percpu.vmcs_revision_id, false)?,
            msr_bitmap: MsrBitmap::passthrough_all()?,
            apic_timer: ApicTimer::new(),
            pending_events: VecDeque::with_capacity(8),
            is_launched: false,
            cpu_mode: VmCpuMode::Real,
            nr_sipi: 0,
        };
        vcpu.setup_msr_bitmap()?;
        vcpu.setup_vmcs(Some(entry), ept_root)?;
        info!("[HV] created VmxVcpu(vmcs: {:#x})", vcpu.vmcs.phys_addr());
        Ok(vcpu)
    }

    /// Run the guest. It returns when a vm-exit happens and returns the vm-exit if it cannot be handled by this [`VmxVcpu`] itself.
    pub fn run(&mut self) -> Option<VmxExitInfo> {
        // FIXME
        // if self.is_launched {
        //     self.inject_pending_events().unwrap();
        // }
        // unsafe {
        //     if self.is_launched {
        //         self.vmx_resume();
        //     } else {
        //         self.is_launched = true;
        //         VmcsHostNW::RSP
        //             .write(&self.host_stack_top as *const _ as usize)
        //             .unwrap();
        //         self.vmx_launch();
        //     }
        // }

        unsafe {
            self.inject_pending_events().unwrap();
            if self.is_launched {
                self.vmx_resume();
            } else {
                self.is_launched = true;
                self.vmx_launch();
            }
        }
        // handle vm-exits
        let exit_info = self.exit_info().unwrap();
        trace!("VM exit: {:#x?}",exit_info);
        match self.builtin_vmexit_handler(&exit_info) {
            None => {
                Some(exit_info)
            }
            Some(res) => {
                if res.is_err() {
                    panic!("VmxVcpu failed to handle a VM-exit that should be handled by itself");
                }
                None
            }
        }
    }

    /// Basic information about VM exits.
    pub fn exit_info(&self) -> HyperResult<vmcs::VmxExitInfo> {
        vmcs::exit_info()
    }

    pub fn raw_interrupt_exit_info(&self) -> HyperResult<u32> {
        vmcs::raw_interrupt_exit_info()
    }

    /// Information for VM exits due to external interrupts.
    pub fn interrupt_exit_info(&self) -> HyperResult<vmcs::VmxInterruptInfo> {
        vmcs::interrupt_exit_info()
    }

    /// Information for VM exits due to I/O instructions.
    pub fn io_exit_info(&self) -> HyperResult<vmcs::VmxIoExitInfo> {
        vmcs::io_exit_info()
    }

    /// Information for VM exits due to nested page table faults (EPT violation).
    pub fn nested_page_fault_info(&self) -> HyperResult<NestedPageFaultInfo> {
        vmcs::ept_violation_info()
    }

    /// Guest general-purpose registers.
    pub fn regs(&self) -> &GeneralRegisters {
        &self.guest_regs
    }

    /// Mutable reference of guest general-purpose registers.
    pub fn regs_mut(&mut self) -> &mut GeneralRegisters {
        &mut self.guest_regs
    }

    /// Guest stack pointer. (`RSP`)
    pub fn stack_pointer(&self) -> usize {
        VmcsGuestNW::RSP.read().unwrap()
    }

    /// Set guest stack pointer. (`RSP`)
    pub fn set_stack_pointer(&mut self, rsp: usize) {
        VmcsGuestNW::RSP.write(rsp).unwrap()
    }

    /// Guest rip. (`RIP`)
    pub fn rip(&self) -> usize {
        VmcsGuestNW::RIP.read().unwrap()
    }

    /// Guest cs. (`cs`)
    pub fn cs(&self) -> u16 {
        VmcsGuest16::CS_SELECTOR.read().unwrap()
    }

    /// Advance guest `RIP` by `instr_len` bytes.
    pub fn advance_rip(&mut self, instr_len: u8) -> HyperResult {
        Ok(VmcsGuestNW::RIP.write(VmcsGuestNW::RIP.read()? + instr_len as usize)?)
    }

    /// Add a virtual interrupt or exception to the pending events list,
    /// and try to inject it before later VM entries.
    pub fn queue_event(&mut self, vector: u8, err_code: Option<u32>) {
        self.pending_events.push_back((vector, err_code));
    }

    /// If enabled, a VM exit occurs at the beginning of any instruction if
    /// `RFLAGS.IF` = 1 and there are no other blocking of interrupts.
    /// (see SDM, Vol. 3C, Section 24.4.2)
    pub fn set_interrupt_window(&mut self, enable: bool) -> HyperResult {
        let mut ctrl = VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS.read()?;
        let bits = vmcs::controls::PrimaryControls::INTERRUPT_WINDOW_EXITING.bits();
        if enable {
            ctrl |= bits
        } else {
            ctrl &= !bits
        }
        VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS.write(ctrl)?;
        Ok(())
    }

    /// Set msr intercept by modifying msr bitmap.
    /// Todo: distinguish read and write.
    pub fn set_msr_intercept_of_range(&mut self, msr: u32, intercept: bool) {
        self.msr_bitmap.set_read_intercept(msr, intercept);
        self.msr_bitmap.set_write_intercept(msr, intercept);
    }

    /// Returns the mutable reference of [`ApicTimer`].
    pub fn apic_timer_mut(&mut self) -> &mut ApicTimer<H> {
        &mut self.apic_timer
    }
}

// Implementation of private methods
impl<H: HyperCraftHal> VmxVcpu<H> {
    fn setup_msr_bitmap(&mut self) -> HyperResult {
        // Intercept IA32_APIC_BASE MSR accesses
        let msr = x86::msr::IA32_APIC_BASE;
        self.msr_bitmap.set_read_intercept(msr, true);
        self.msr_bitmap.set_write_intercept(msr, true);
        // Intercept all x2APIC MSR accesses
        for msr in 0x800..=0x83f {
            self.msr_bitmap.set_read_intercept(msr, true);
            self.msr_bitmap.set_write_intercept(msr, true);
        }
        Ok(())
    }

    /// setup_vmcs
    pub fn setup_vmcs(&mut self, entry: Option<GuestPhysAddr>, ept_root: HostPhysAddr) -> HyperResult {
        self.setup_vmcs_host()?;
        self.setup_vmcs_guest(entry)?;
        self.setup_vmcs_control(ept_root)?;
        Ok(())
    }

    fn setup_vmcs_host(&mut self) -> HyperResult {
        VmcsHost64::IA32_PAT.write(Msr::IA32_PAT.read())?;
        VmcsHost64::IA32_EFER.write(Msr::IA32_EFER.read())?;

        VmcsHostNW::CR0.write(Cr0::read_raw() as _)?;
        VmcsHostNW::CR3.write(Cr3::read_raw().0.start_address().as_u64() as _)?;
        VmcsHostNW::CR4.write(Cr4::read_raw() as _)?;

        VmcsHost16::ES_SELECTOR.write(x86::segmentation::es().bits())?;
        VmcsHost16::CS_SELECTOR.write(x86::segmentation::cs().bits())?;
        VmcsHost16::SS_SELECTOR.write(x86::segmentation::ss().bits())?;
        VmcsHost16::DS_SELECTOR.write(x86::segmentation::ds().bits())?;
        VmcsHost16::FS_SELECTOR.write(x86::segmentation::fs().bits())?;
        VmcsHost16::GS_SELECTOR.write(x86::segmentation::gs().bits())?;
        VmcsHostNW::FS_BASE.write(Msr::IA32_FS_BASE.read() as _)?;
        VmcsHostNW::GS_BASE.write(Msr::IA32_GS_BASE.read() as _)?;

        let tr = unsafe { x86::task::tr() };
        let mut gdtp = DescriptorTablePointer::<u64>::default();
        let mut idtp = DescriptorTablePointer::<u64>::default();
        unsafe {
            dtables::sgdt(&mut gdtp);
            dtables::sidt(&mut idtp);
        }
        VmcsHost16::TR_SELECTOR.write(tr.bits())?;
        VmcsHostNW::TR_BASE.write(get_tr_base(tr, &gdtp) as _)?;
        VmcsHostNW::GDTR_BASE.write(gdtp.base as _)?;
        VmcsHostNW::IDTR_BASE.write(idtp.base as _)?;
        VmcsHostNW::RIP.write(Self::vmx_exit as usize)?;

        VmcsHostNW::IA32_SYSENTER_ESP.write(0)?;
        VmcsHostNW::IA32_SYSENTER_EIP.write(0)?;
        VmcsHost32::IA32_SYSENTER_CS.write(0)?;

        VmcsHostNW::RSP
            .write(&self.host_stack_top as *const _ as usize)
            .unwrap();

        Ok(())
    }

    fn setup_vmcs_guest(&mut self, entry: Option<GuestPhysAddr>) -> HyperResult {
        let cr0_guest = Cr0Flags::EXTENSION_TYPE | Cr0Flags::NUMERIC_ERROR;
        let cr0_host_owned =
            Cr0Flags::NUMERIC_ERROR | Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE;
        let cr0_read_shadow = Cr0Flags::NUMERIC_ERROR;
        VmcsGuestNW::CR0.write(cr0_guest.bits() as _)?;
        VmcsControlNW::CR0_GUEST_HOST_MASK.write(cr0_host_owned.bits() as _)?;
        VmcsControlNW::CR0_READ_SHADOW.write(cr0_read_shadow.bits() as _)?;

        let cr4_guest = Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS;
        let cr4_host_owned = Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS;
        let cr4_read_shadow = 0;
        VmcsGuestNW::CR4.write(cr4_guest.bits() as _)?;
        VmcsControlNW::CR4_GUEST_HOST_MASK.write(cr4_host_owned.bits() as _)?;
        VmcsControlNW::CR4_READ_SHADOW.write(cr4_read_shadow)?;

        macro_rules! set_guest_segment {
            ($seg: ident, $access_rights: expr) => {{
                use VmcsGuest16::*;
                use VmcsGuest32::*;
                use VmcsGuestNW::*;
                concat_idents!($seg, _SELECTOR).write(0)?;
                concat_idents!($seg, _BASE).write(0)?;
                concat_idents!($seg, _LIMIT).write(0xffff)?;
                concat_idents!($seg, _ACCESS_RIGHTS).write($access_rights)?;
            }};
        }

        set_guest_segment!(ES, 0x93); // 16-bit, present, data, read/write, accessed
        set_guest_segment!(CS, 0x9b); // 16-bit, present, code, exec/read, accessed
        set_guest_segment!(SS, 0x93);
        set_guest_segment!(DS, 0x93);
        set_guest_segment!(FS, 0x93);
        set_guest_segment!(GS, 0x93);
        set_guest_segment!(TR, 0x8b); // present, system, 32-bit TSS busy
        set_guest_segment!(LDTR, 0x82); // present, system, LDT

        VmcsGuestNW::GDTR_BASE.write(0)?;
        VmcsGuest32::GDTR_LIMIT.write(0xffff)?;
        VmcsGuestNW::IDTR_BASE.write(0)?;
        VmcsGuest32::IDTR_LIMIT.write(0xffff)?;

        VmcsGuestNW::CR3.write(0)?;
        VmcsGuestNW::DR7.write(0x400)?;
        VmcsGuestNW::RSP.write(0)?;

        if let Some(entry) = entry {
            VmcsGuestNW::RIP.write(entry)?;
        }
        VmcsGuestNW::RFLAGS.write(0x2)?;
        VmcsGuestNW::PENDING_DBG_EXCEPTIONS.write(0)?;
        VmcsGuestNW::IA32_SYSENTER_ESP.write(0)?;
        VmcsGuestNW::IA32_SYSENTER_EIP.write(0)?;
        VmcsGuest32::IA32_SYSENTER_CS.write(0)?;

        VmcsGuest32::INTERRUPTIBILITY_STATE.write(0)?;
        VmcsGuest32::ACTIVITY_STATE.write(0)?;
        // set timer
        VmcsGuest32::VMX_PREEMPTION_TIMER_VALUE.write(unsafe { VMX_PREEMPTION_TIMER_SET_VALUE })?;

        VmcsGuest64::LINK_PTR.write(u64::MAX)?; // SDM Vol. 3C, Section 24.4.2
        VmcsGuest64::IA32_DEBUGCTL.write(0)?;
        VmcsGuest64::IA32_PAT.write(Msr::IA32_PAT.read())?;
        VmcsGuest64::IA32_EFER.write(0)?;
        Ok(())
    }

    fn setup_vmcs_control(&mut self, ept_root: HostPhysAddr) -> HyperResult {
        // Intercept NMI and external interrupts.
        use super::vmcs::controls::*;
        use PinbasedControls as PinCtrl;
        vmcs::set_control(
            VmcsControl32::PINBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_TRUE_PINBASED_CTLS,
            Msr::IA32_VMX_PINBASED_CTLS.read() as u32,
            (PinCtrl::NMI_EXITING | PinCtrl::EXTERNAL_INTERRUPT_EXITING | PinCtrl::VMX_PREEMPTION_TIMER).bits(),
            0,
        )?;

        // Intercept all I/O instructions, use MSR bitmaps, activate secondary controls,
        // disable CR3 load/store interception.
        use PrimaryControls as CpuCtrl;
        vmcs::set_control(
            VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_TRUE_PROCBASED_CTLS,
            Msr::IA32_VMX_PROCBASED_CTLS.read() as u32,
            (CpuCtrl::UNCOND_IO_EXITING | CpuCtrl::USE_MSR_BITMAPS | CpuCtrl::SECONDARY_CONTROLS)
                .bits(),
            (CpuCtrl::CR3_LOAD_EXITING | CpuCtrl::CR3_STORE_EXITING).bits(),
        )?;

        // Enable EPT, RDTSCP, INVPCID, and unrestricted guest.
        use SecondaryControls as CpuCtrl2;
        vmcs::set_control(
            VmcsControl32::SECONDARY_PROCBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_PROCBASED_CTLS2,
            0,
            (CpuCtrl2::ENABLE_EPT
                | CpuCtrl2::ENABLE_RDTSCP
                | CpuCtrl2::ENABLE_INVPCID
                | CpuCtrl2::UNRESTRICTED_GUEST)
                .bits(),
            0,
        )?;

        // Switch to 64-bit host, acknowledge interrupt info, switch IA32_PAT/IA32_EFER on VM exit.
        use ExitControls as ExitCtrl;
        vmcs::set_control(
            VmcsControl32::VMEXIT_CONTROLS,
            Msr::IA32_VMX_TRUE_EXIT_CTLS,
            Msr::IA32_VMX_EXIT_CTLS.read() as u32,
            (ExitCtrl::HOST_ADDRESS_SPACE_SIZE
                | ExitCtrl::ACK_INTERRUPT_ON_EXIT
                | ExitCtrl::SAVE_IA32_PAT
                | ExitCtrl::LOAD_IA32_PAT
                | ExitCtrl::SAVE_IA32_EFER
                | ExitCtrl::LOAD_IA32_EFER)
                .bits(),
            0,
        )?;

        // Load guest IA32_PAT/IA32_EFER on VM entry.
        use EntryControls as EntryCtrl;
        vmcs::set_control(
            VmcsControl32::VMENTRY_CONTROLS,
            Msr::IA32_VMX_TRUE_ENTRY_CTLS,
            Msr::IA32_VMX_ENTRY_CTLS.read() as u32,
            (EntryCtrl::LOAD_IA32_PAT | EntryCtrl::LOAD_IA32_EFER).bits(),
            0,
        )?;

        vmcs::set_ept_pointer(ept_root)?;

        // No MSR switches if hypervisor doesn't use and there is only one vCPU.
        VmcsControl32::VMEXIT_MSR_STORE_COUNT.write(0)?;
        VmcsControl32::VMEXIT_MSR_LOAD_COUNT.write(0)?;
        VmcsControl32::VMENTRY_MSR_LOAD_COUNT.write(0)?;

        // Pass-through exceptions, don't use I/O bitmap, set MSR bitmaps.
        VmcsControl32::EXCEPTION_BITMAP.write(0)?;
        VmcsControl64::IO_BITMAP_A_ADDR.write(0)?;
        VmcsControl64::IO_BITMAP_B_ADDR.write(0)?;
        VmcsControl64::MSR_BITMAPS_ADDR.write(self.msr_bitmap.phys_addr() as _)?;
        Ok(())
    }
}

/// Get ready then vmlaunch or vmresume.
macro_rules! vmx_entry_with {
    ($instr:literal) => {
        asm!(
            save_regs_to_stack!(),                  // save host status
            "mov    [rdi + {host_stack_top}], rsp", // save current RSP to Vcpu::host_stack_top
            "mov    rsp, rdi",                      // set RSP to guest regs area
            restore_regs_from_stack!(),             // restore guest status
            $instr,                                 // let's go!
            "jmp    {failed}",
            host_stack_top = const size_of::<GeneralRegisters>(),
            failed = sym Self::vmx_entry_failed,
            options(noreturn),
        )
    }
}


impl<H: HyperCraftHal> VmxVcpu<H> {
    /// Enter guest with vmlaunch.
    ///
    /// `#[naked]` is essential here, without it the rust compiler will think `&mut self` is not used and won't give us correct %rdi.
    ///
    /// This function itself never returns, but [`Self::vmx_exit`] will do the return for this.
    ///
    /// The return value is a dummy value.
    #[naked]
    unsafe extern "C" fn vmx_launch(&mut self) {
        vmx_entry_with!("vmlaunch")
    }
    /// Enter guest with vmresume.
    ///
    /// See [`Self::vmx_launch`] for detail.
    #[naked]
    unsafe extern "C" fn vmx_resume(&mut self) {
        vmx_entry_with!("vmresume")
    }
    /// Return after vm-exit.
    ///
    /// The return value is a dummy value.
    #[naked]
    unsafe extern "C" fn vmx_exit(&mut self) {
        asm!(
            save_regs_to_stack!(),
            "mov    rsp, [rsp + {host_stack_top}]", // set RSP to Vcpu::host_stack_top
            restore_regs_from_stack!(),
            "ret",
            host_stack_top = const size_of::<GeneralRegisters>(),
            options(noreturn),
        )
    }

    fn vmx_entry_failed() -> ! {
        panic!("{}", vmcs::instruction_error().as_str())
    }

    /// Whether the guest interrupts are blocked. (SDM Vol. 3C, Section 24.4.2, Table 24-3)
    fn allow_interrupt(&self) -> bool {
        let rflags = VmcsGuestNW::RFLAGS.read().unwrap();
        let block_state = VmcsGuest32::INTERRUPTIBILITY_STATE.read().unwrap();
        rflags as u64 & x86_64::registers::rflags::RFlags::INTERRUPT_FLAG.bits() != 0
            && block_state == 0
    }

    /// Try to inject a pending event before next VM entry.
    fn inject_pending_events(&mut self) -> HyperResult {
        if let Some(event) = self.pending_events.front() {
            if event.0 < 32 || self.allow_interrupt() {
                // if it's an exception, or an interrupt that is not blocked, inject it directly.
                vmcs::inject_event(event.0, event.1)?;
                self.pending_events.pop_front();
            } else {
                // interrupts are blocked, enable interrupt-window exiting.
                self.set_interrupt_window(true)?;
            }
        }
        Ok(())
    }

    /// Handle vm-exits than can and should be handled by [`VmxVcpu`] itself.
    ///
    /// Return the result or None if the vm-exit was not handled.
    fn builtin_vmexit_handler(&mut self, exit_info: &VmExitInfo) -> Option<HyperResult> {
        if exit_info.entry_failure {
            panic!("VM entry failed: {:#x?}", exit_info);
        }


        // Following vm-exits are handled here:
        // - interrupt window: turn off interrupt window;
        // - xsetbv: set guest xcr;
        // - cr access: just panic;
        let ret = match exit_info.exit_reason {
            VmxExitReason::INTERRUPT_WINDOW => Some(self.set_interrupt_window(false)),
            // VmxExitReason::XSETBV => Some(self.handle_xsetbv()),
            VmxExitReason::CR_ACCESS => Some(self.handle_cr()),
            VmxExitReason::CPUID => Some(self.handle_cpuid()),
            _ => None,
        };

        // Check if there is an APIC timer interrupt
        if self.apic_timer.check_interrupt() {
            self.queue_event(self.apic_timer.vector(), None);
        }
        self.inject_pending_events().unwrap();
        ret
    }

    pub fn reset_timer(&mut self) -> HyperResult {
        /*
           The VMX-preemption timer counts down at rate proportional to that of the timestamp counter (TSC).
           Specifically, the timer counts down by 1 every time bit X in the TSC changes due to a TSC increment.
           The value of X is in the range 0–31 and can be determined by consulting the VMX capability MSR IA32_VMX_MISC (see Appendix A.6).
        */
        error!("reset vmx timer");
        VmcsGuest32::VMX_PREEMPTION_TIMER_VALUE.write(unsafe { VMX_PREEMPTION_TIMER_SET_VALUE })?;
        Ok(())
    }

    fn set_cr(&mut self, cr_idx: usize, val: u64) {
        (|| -> HyperResult {
            // debug!("set guest CR{} to val {:#x}", cr_idx, val);
            match cr_idx {
                0 => {
                    // Retrieve/validate restrictions on CR0
                    //
                    // In addition to what the VMX MSRs tell us, make sure that
                    // - NW and CD are kept off as they are not updated on VM exit and we
                    //   don't want them enabled for performance reasons while in root mode
                    // - PE and PG can be freely chosen (by the guest) because we demand
                    //   unrestricted guest mode support anyway
                    // - ET is ignored
                    let must0 = Msr::IA32_VMX_CR0_FIXED1.read()
                        & !(Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE).bits();
                    let must1 = Msr::IA32_VMX_CR0_FIXED0.read()
                        & !(Cr0Flags::PAGING | Cr0Flags::PROTECTED_MODE_ENABLE).bits();
                    VmcsGuestNW::CR0.write(((val & must0) | must1) as _)?;
                    VmcsControlNW::CR0_READ_SHADOW.write(val as _)?;
                    VmcsControlNW::CR0_GUEST_HOST_MASK.write((must1 | !must0) as _)?;
                }
                3 => VmcsGuestNW::CR3.write(val as _)?,
                4 => {
                    // Retrieve/validate restrictions on CR4
                    let must0 = Msr::IA32_VMX_CR4_FIXED1.read();
                    let must1 = Msr::IA32_VMX_CR4_FIXED0.read();
                    let val = val | Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS.bits();
                    VmcsGuestNW::CR4.write(((val & must0) | must1) as _)?;
                    VmcsControlNW::CR4_READ_SHADOW.write(val as _)?;
                    VmcsControlNW::CR4_GUEST_HOST_MASK.write((must1 | !must0) as _)?;
                }
                _ => unreachable!(),
            };
            Ok(())
        })()
            .expect("Failed to write guest control register")
    }


    fn handle_cr(&mut self) -> HyperResult {
        const VM_EXIT_INSTR_LEN_MV_TO_CR: u8 = 3;

        let cr_access_info = vmcs::cr_access_info()?;

        let reg = cr_access_info.gpr;
        let cr = cr_access_info.cr_number;

        match cr_access_info.access_type {
            /* move to cr */
            0 => {
                let val = if reg == 4 {
                    self.stack_pointer() as u64
                } else {
                    self.guest_regs.get_reg_of_index(reg)
                };
                if cr == 0 || cr == 4 {
                    self.advance_rip(VM_EXIT_INSTR_LEN_MV_TO_CR)?;
                    /* TODO: check for #GP reasons */
                    self.set_cr(cr as usize, val);

                    if cr == 0 && Cr0Flags::from_bits_truncate(val).contains(Cr0Flags::PAGING) {
                        vmcs::update_efer()?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        };

        panic!(
            "Guest's access to cr not allowed: {:#x?}, {:#x?}",
            self, cr_access_info
        );
    }

    fn handle_cpuid(&mut self) -> HyperResult {
        use raw_cpuid::{cpuid, CpuIdResult};

        const VM_EXIT_INSTR_LEN_CPUID: u8 = 2;
        const LEAF_FEATURE_INFO: u32 = 0x1;
        const LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION: u32 = 0x7;
        const LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION: u32 = 0xd;
        const EAX_FREQUENCY_INFO: u32 = 0x16;
        const LEAF_HYPERVISOR_INFO: u32 = 0x4000_0000;
        const LEAF_HYPERVISOR_FEATURE: u32 = 0x4000_0001;
        const VENDOR_STR: &[u8; 12] = b"RVMRVMRVMRVM";
        let vendor_regs = unsafe { &*(VENDOR_STR.as_ptr() as *const [u32; 3]) };

        let regs_clone = self.regs_mut().clone();
        let function = regs_clone.rax as u32;
        let res = match function {
            LEAF_FEATURE_INFO => {
                const FEATURE_VMX: u32 = 1 << 5;
                const FEATURE_HYPERVISOR: u32 = 1 << 31;
                const FEATURE_MCE: u32 = 1 << 7;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                debug!("set cpu id as vcpu id {}", self.vcpu_id);
                res.ebx.set_bits(24..=31, self.vcpu_id as u32);
                res.ecx &= !FEATURE_VMX;
                res.ecx |= FEATURE_HYPERVISOR;
                res.eax &= !FEATURE_MCE;
                res
            }
            // See SDM Table 3-8. Information Returned by CPUID Instruction (Contd.)
            LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION => {
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if regs_clone.rcx == 0 {
                    // Bit 05: WAITPKG.
                    res.ecx.set_bit(5, false); // clear waitpkg
                    // Bit 16: LA57. Supports 57-bit linear addresses and five-level paging if 1.
                    res.ecx.set_bit(16, false); // clear LA57
                }

                res
            }
            LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION => {
                let res = cpuid!(regs_clone.rax, regs_clone.rcx);
                res
            }
            LEAF_HYPERVISOR_INFO => CpuIdResult {
                eax: LEAF_HYPERVISOR_FEATURE,
                ebx: vendor_regs[0],
                ecx: vendor_regs[1],
                edx: vendor_regs[2],
            },
            LEAF_HYPERVISOR_FEATURE => CpuIdResult {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
            EAX_FREQUENCY_INFO => {
                /// Timer interrupt frequency Hz.
                /// Todo: this should be the same as `axconfig::TIMER_FREQUENCY` defined in ArceOS's config file.
                const TIMER_FREQUENCY_MHz: u32 = 3_000;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if res.eax == 0 {
                    warn!(
                        "handle_cpuid: Failed to get TSC frequency by CPUID, default to {} MHz",
                        TIMER_FREQUENCY_MHz
                    );
                    res.eax = TIMER_FREQUENCY_MHz;
                }
                res
            }
            _ => cpuid!(regs_clone.rax, regs_clone.rcx),
        };

        trace!(
            "VM exit: CPUID({:#x}, {:#x}): {:?}",
            regs_clone.rax,
            regs_clone.rcx,
            res
        );

        let regs = self.regs_mut();
        regs.rax = res.eax as _;
        regs.rbx = res.ebx as _;
        regs.rcx = res.ecx as _;
        regs.rdx = res.edx as _;
        self.advance_rip(VM_EXIT_INSTR_LEN_CPUID)?;

        Ok(())
    }
}


impl<H: HyperCraftHal> Drop for VmxVcpu<H> {
    fn drop(&mut self) {
        unsafe { vmx::vmclear(self.vmcs.phys_addr() as u64).unwrap() };
        info!("[HV] dropped VmxVcpu(vmcs: {:#x})", self.vmcs.phys_addr());
    }
}

fn get_tr_base(tr: SegmentSelector, gdt: &DescriptorTablePointer<u64>) -> u64 {
    let index = tr.index() as usize;
    let table_len = (gdt.limit as usize + 1) / core::mem::size_of::<u64>();
    let table = unsafe { core::slice::from_raw_parts(gdt.base, table_len) };
    let entry = table[index];
    if entry & (1 << 47) != 0 {
        // present
        let base_low = entry.get_bits(16..40) | entry.get_bits(56..64) << 24;
        let base_high = table[index + 1] & 0xffff_ffff;
        base_low | base_high << 32
    } else {
        // no present
        0
    }
}

impl<H: HyperCraftHal> Debug for VmxVcpu<H> {
    fn fmt(&self, f: &mut Formatter) -> Result {
        (|| -> HyperResult<Result> {
            Ok(f.debug_struct("VmxVcpu")
                .field("guest_regs", &self.guest_regs)
                .field("rip", &VmcsGuestNW::RIP.read()?)
                .field("rsp", &VmcsGuestNW::RSP.read()?)
                .field("rflags", &VmcsGuestNW::RFLAGS.read()?)
                .field("cr0", &VmcsGuestNW::CR0.read()?)
                .field("cr3", &VmcsGuestNW::CR3.read()?)
                .field("cr4", &VmcsGuestNW::CR4.read()?)
                .field("cs", &VmcsGuest16::CS_SELECTOR.read()?)
                .field("fs_base", &VmcsGuestNW::FS_BASE.read()?)
                .field("gs_base", &VmcsGuestNW::GS_BASE.read()?)
                .field("tss", &VmcsGuest16::TR_SELECTOR.read()?)
                .finish())
        })()
            .unwrap()
    }
}
