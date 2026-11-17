#!/Linux 串口 stub（将来 lin door 实现）
use crate::core::protocol::{Task, Res};

pub fn run_serial(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial: not supported on linux build".into(), ms: 0 }
}
pub fn run_serial_open(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial_open: not supported on linux build".into(), ms: 0 }
}
pub fn run_serial_write(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial_write: not supported on linux build".into(), ms: 0 }
}
pub fn run_serial_close(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial_close: not supported on linux build".into(), ms: 0 }
}
