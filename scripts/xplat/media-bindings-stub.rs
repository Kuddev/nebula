// 交叉 `cargo check --target aarch64-apple-darwin` 用的 CoreMedia 绑定桩。
//
// zed 的 `media` crate 在 build.rs 里用 bindgen + `xcrun` 从 macOS SDK 生成
// `bindings.rs`，而且 build.rs 的 `#[cfg(target_os = "macos")]` 看的是**宿主**，
// 所以在 Windows 上交叉 check 时它什么都不生成。这里手写它实际用到的那几个
// 符号（类型布局与 CoreMedia 一致），只为让 Rust 层通过类型检查。
// 绝不能进入真实 macOS 构建——那里由 bindgen 产出真货。


pub type CFStringRef = *const core::ffi::c_void;
pub type CVReturn = i32;
pub type CMItemIndex = i64;
pub type CMTimeValue = i64;
pub type CMTimeScale = i32;
pub type CMTimeFlags = u32;
pub type CMTimeEpoch = i64;
pub type CMVideoCodecType = u32;
pub type VTEncodeInfoFlags = u32;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CMTime {
    pub value: CMTimeValue,
    pub timescale: CMTimeScale,
    pub flags: CMTimeFlags,
    pub epoch: CMTimeEpoch,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CMSampleTimingInfo {
    pub duration: CMTime,
    pub presentationTimeStamp: CMTime,
    pub decodeTimeStamp: CMTime,
}

pub const VTEncodeInfoFlags_kVTEncodeInfo_Asynchronous: VTEncodeInfoFlags = 1;
pub const VTEncodeInfoFlags_kVTEncodeInfo_FrameDropped: VTEncodeInfoFlags = 2;
pub const kCVReturnSuccess: CVReturn = 0;
pub const kCVPixelFormatType_32BGRA: u32 = 0x4247_5241; // 'BGRA'
pub const kCVPixelFormatType_420YpCbCr8Planar: u32 = 0x7934_3230; // 'y420'
pub const kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: u32 = 0x3432_3076; // '420v'
pub const kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: u32 = 0x3432_3066; // '420f'
pub const kCMVideoCodecType_H264: CMVideoCodecType = 0x6176_6331; // 'avc1'

unsafe extern "C" {
    pub static kCMTimeInvalid: CMTime;
    pub static kCMSampleAttachmentKey_NotSync: CFStringRef;
    pub fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
}
