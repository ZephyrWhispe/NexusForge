//! VirtualLock 内存锁页端口（D-24）：防 DEK 被换入页文件。
//!
//! 契约：成败均以 bool 返回，调用方（vault-core）失败仅 warn 不阻断——
//! 锁页失败是隐私降级，不是功能故障。进程配额（SetProcessWorkingSetSize）
//! 未调整时 VirtualLock 可能因超出 quotalimit 失败，同样落入该降级路径。

use windows::Win32::System::Memory::{VirtualLock, VirtualUnlock};

use host_core::ports::MemLockPort;

pub struct WinMemLock;

impl MemLockPort for WinMemLock {
    fn lock(&self, addr: usize, len: usize) -> bool {
        // SAFETY: 调用方承诺 [addr, addr+len) 是本进程已提交的私有内存
        unsafe { VirtualLock(addr as *const core::ffi::c_void, len).is_ok() }
    }

    fn unlock(&self, addr: usize, len: usize) -> bool {
        unsafe { VirtualUnlock(addr as *const core::ffi::c_void, len).is_ok() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_unlock_roundtrip_on_real_buffer() {
        let buf = vec![0u8; 4096];
        let port = WinMemLock;
        let ok = port.lock(buf.as_ptr() as usize, buf.len());
        if ok {
            assert!(port.unlock(buf.as_ptr() as usize, buf.len()));
        }
        // CI 机器可能限制锁页配额：失败也必须是 false 返回而非 panic（降级契约）
        // 未映射地址必须返回 false，不得崩溃
        assert!(!port.lock(0x1, 16));
    }
}
