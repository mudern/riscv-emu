use riscv_emu::decode::{decode, Inst, SystemOp};

#[test]
fn sfence_decodes() {
    assert_eq!(
        decode(0x1200_0073), // sfence.vma（rs1=rs2=x0）
        Ok(Inst::System(SystemOp::SfenceVma))
    );
    // sfence.vma a0, a1（rs1/rs2 非 0）
    assert_eq!(
        decode(0x1200_0073 | (10 << 15) | (11 << 20)),
        Ok(Inst::System(SystemOp::SfenceVma))
    );
}
