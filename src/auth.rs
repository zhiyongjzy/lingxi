//! PAM 认证模块 — 验证用户密码 (用 pam-sys 0.5)

use pam_sys::types::*;
use pam_sys::wrapped;
use std::os::raw::{c_int, c_void};
use std::ptr;

/// 验证当前用户的密码
pub fn verify_password(password: &str) -> Result<(), String> {
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".into());

    let pw = std::ffi::CString::new(password).map_err(|_| "密码含非法字符".to_string())?;

    let conv = PamConversation {
        conv: Some(safe_pam_conversation),
        data_ptr: pw.as_ptr() as *mut c_void,
    };

    let mut handle: *mut PamHandle = ptr::null_mut();

    tracing::info!("🔒 PAM verify: user={}, pw_len={}", username, password.len());

    // 1. pam_start (用 waylock 的 PAM 配置: "auth include system-auth")
    let ret = wrapped::start("waylock", Some(&username), &conv, &mut handle);
    if ret != PamReturnCode::SUCCESS {
        return Err(format!("PAM 初始化失败: {:?}", ret));
    }

    // 2. pam_authenticate
    let handle_ref = unsafe { &mut *handle };
    let ret = wrapped::authenticate(handle_ref, PamFlag::NONE);
    if ret != PamReturnCode::SUCCESS {
        wrapped::end(handle_ref, ret);
        return Err("密码错误".into());
    }

    // 3. pam_acct_mgmt
    let ret = wrapped::acct_mgmt(handle_ref, PamFlag::NONE);
    if ret != PamReturnCode::SUCCESS {
        wrapped::end(handle_ref, ret);
        return Err(format!("账户验证失败: {:?}", ret));
    }

    // 4. pam_end
    wrapped::end(handle_ref, PamReturnCode::SUCCESS);
    Ok(())
}

/// PAM conversation 回调 — 把预存的密码返回给 PAM
/// 注意: pam-sys 0.5 的 PamConversation.conv 期望 safe fn (非 unsafe extern "C")
extern "C" fn safe_pam_conversation(
    num_msg: c_int,
    msg: *mut *mut PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    unsafe {
        let password = appdata_ptr as *const std::os::raw::c_char;

        let responses = libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if responses.is_null() {
            return PamReturnCode::BUF_ERR as c_int;
        }

        for i in 0..num_msg as isize {
            let m = *msg.offset(i);
            let msg_style = (*m).msg_style;
            // PAM_PROMPT_ECHO_OFF = 1
            if msg_style == 1 {
                (*responses.offset(i)).resp = libc::strdup(password);
            }
        }

        *resp = responses;
        PamReturnCode::SUCCESS as c_int
    }
}
