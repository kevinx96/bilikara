//! Narrow JNI boundary for private, user-selected grayscale images.
use jni::{
    JNIEnv,
    objects::{JByteArray, JClass},
    sys::{jint, jstring},
};

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_bilikara_app_HostCapture_decodeLuma(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    pixels: JByteArray<'_>,
    width: jint,
    height: jint,
) -> jstring {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !(1..=2048).contains(&width)
            || !(1..=2048).contains(&height)
            || env.get_array_length(&pixels).ok() != Some(width * height)
        {
            return Err("cast_capture_invalid");
        }
        let bytes = env
            .convert_byte_array(&pixels)
            .map_err(|_| "cast_capture_invalid")?;
        bilikara_runtime::decode_qr_luma(width as usize, height as usize, &bytes)
    }))
    .unwrap_or(Err("cast_capture_invalid"));
    let value = match result {
        Ok(text) => serde_json::json!({"text":text}),
        Err(code) => serde_json::json!({"error":code}),
    };
    env.new_string(value.to_string())
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
