/// sifive_test 设备（virt 机器布局在 0x0010_0000），用于让 guest 主动关机。
/// 写 0x5555 成功退出（退出码取高 16 位），写 0x3333 失败退出，0x7777 视作复位关机。
#[derive(Default)]
pub struct TestDevice {
    pub exit: Option<i32>,
}

impl TestDevice {
    pub fn new() -> Self {
        TestDevice { exit: None }
    }

    pub fn store(&mut self, val: u64) {
        let code = (val >> 16) as i32;
        self.exit = match val & 0xFFFF {
            0x5555 => Some(code),
            0x3333 => Some(code.saturating_add(1)),
            0x7777 => Some(0),
            _ => self.exit,
        };
    }
}
