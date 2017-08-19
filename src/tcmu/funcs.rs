pub fn tcmu_hdr_get_op(len_op: u32) -> u32 {
    len_op & super::TCMU_OP_MASK
}

pub fn tcmu_hdr_get_len(len_op: u32) -> u32 {
    len_op & !super::TCMU_OP_MASK
}
