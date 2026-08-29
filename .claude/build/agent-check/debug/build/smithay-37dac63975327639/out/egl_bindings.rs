
        mod __gl_imports {
            pub use std::mem;
            pub use std::os::raw;
        }
    

        #[inline(never)]
        fn metaloadfn(loadfn: &mut dyn FnMut(&'static str) -> *const __gl_imports::raw::c_void,
                      symbol: &'static str,
                      fallbacks: &[&'static str]) -> *const __gl_imports::raw::c_void {
            let mut ptr = loadfn(symbol);
            if ptr.is_null() {
                for &sym in fallbacks {
                    ptr = loadfn(sym);
                    if !ptr.is_null() { break; }
                }
            }
            ptr
        }
    

        pub mod types {
            #![allow(non_camel_case_types, non_snake_case, dead_code, missing_copy_implementations)]
    
// platform-specific aliases are unknown
// IMPORTANT: these are alises to the same level of the bindings
// the values must be defined by the user
#[allow(dead_code)]
pub type khronos_utime_nanoseconds_t = super::khronos_utime_nanoseconds_t;
#[allow(dead_code)]
pub type khronos_uint64_t = super::khronos_uint64_t;
#[allow(dead_code)]
pub type khronos_ssize_t = super::khronos_ssize_t;
pub type EGLNativeDisplayType = super::EGLNativeDisplayType;
#[allow(dead_code)]
pub type EGLNativePixmapType = super::EGLNativePixmapType;
#[allow(dead_code)]
pub type EGLNativeWindowType = super::EGLNativeWindowType;
pub type EGLint = super::EGLint;
#[allow(dead_code)]
pub type NativeDisplayType = super::NativeDisplayType;
#[allow(dead_code)]
pub type NativePixmapType = super::NativePixmapType;
#[allow(dead_code)]
pub type NativeWindowType = super::NativeWindowType;

// EGL alises
pub type Bool = EGLBoolean; // TODO: not sure
pub type EGLBoolean = super::__gl_imports::raw::c_uint;
pub type EGLenum = super::__gl_imports::raw::c_uint;
pub type EGLAttribKHR = isize;
pub type EGLAttrib = isize;
pub type EGLConfig = *const super::__gl_imports::raw::c_void;
pub type EGLContext = *const super::__gl_imports::raw::c_void;
pub type EGLDeviceEXT = *const super::__gl_imports::raw::c_void;
pub type EGLDisplay = *const super::__gl_imports::raw::c_void;
pub type EGLSurface = *const super::__gl_imports::raw::c_void;
pub type EGLClientBuffer = *const super::__gl_imports::raw::c_void;
pub enum __eglMustCastToProperFunctionPointerType_fn {}
pub type __eglMustCastToProperFunctionPointerType =
    *mut __eglMustCastToProperFunctionPointerType_fn;
pub type EGLImageKHR = *const super::__gl_imports::raw::c_void;
pub type EGLImage = *const super::__gl_imports::raw::c_void;
pub type EGLOutputLayerEXT = *const super::__gl_imports::raw::c_void;
pub type EGLOutputPortEXT = *const super::__gl_imports::raw::c_void;
pub type EGLSyncKHR = *const super::__gl_imports::raw::c_void;
pub type EGLSync = *const super::__gl_imports::raw::c_void;
pub type EGLTimeKHR = khronos_utime_nanoseconds_t;
pub type EGLTime = khronos_utime_nanoseconds_t;
pub type EGLSyncNV = *const super::__gl_imports::raw::c_void;
pub type EGLTimeNV = khronos_utime_nanoseconds_t;
pub type EGLuint64NV = khronos_utime_nanoseconds_t;
pub type EGLStreamKHR = *const super::__gl_imports::raw::c_void;
pub type EGLuint64KHR = khronos_uint64_t;
pub type EGLNativeFileDescriptorKHR = super::__gl_imports::raw::c_int;
pub type EGLsizeiANDROID = khronos_ssize_t;
pub type EGLSetBlobFuncANDROID = extern "system" fn(*const super::__gl_imports::raw::c_void,
                                                    EGLsizeiANDROID,
                                                    *const super::__gl_imports::raw::c_void,
                                                    EGLsizeiANDROID)
                                                    -> ();
pub type EGLGetBlobFuncANDROID = extern "system" fn(*const super::__gl_imports::raw::c_void,
                                                    EGLsizeiANDROID,
                                                    *mut super::__gl_imports::raw::c_void,
                                                    EGLsizeiANDROID)
                                                    -> EGLsizeiANDROID;

#[repr(C)]
pub struct EGLClientPixmapHI {
    pData: *const super::__gl_imports::raw::c_void,
    iWidth: EGLint,
    iHeight: EGLint,
    iStride: EGLint,
}


        }
    
#[allow(dead_code, non_upper_case_globals)] pub const ALPHA_FORMAT: types::EGLenum = 0x3088;
#[allow(dead_code, non_upper_case_globals)] pub const ALPHA_FORMAT_NONPRE: types::EGLenum = 0x308B;
#[allow(dead_code, non_upper_case_globals)] pub const ALPHA_FORMAT_PRE: types::EGLenum = 0x308C;
#[allow(dead_code, non_upper_case_globals)] pub const ALPHA_MASK_SIZE: types::EGLenum = 0x303E;
#[allow(dead_code, non_upper_case_globals)] pub const ALPHA_SIZE: types::EGLenum = 0x3021;
#[allow(dead_code, non_upper_case_globals)] pub const BACK_BUFFER: types::EGLenum = 0x3084;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_ACCESS: types::EGLenum = 0x3002;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_ALLOC: types::EGLenum = 0x3003;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_ATTRIBUTE: types::EGLenum = 0x3004;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_CONFIG: types::EGLenum = 0x3005;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_CONTEXT: types::EGLenum = 0x3006;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_CURRENT_SURFACE: types::EGLenum = 0x3007;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_DEVICE_EXT: types::EGLenum = 0x322B;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_DISPLAY: types::EGLenum = 0x3008;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_MATCH: types::EGLenum = 0x3009;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_NATIVE_PIXMAP: types::EGLenum = 0x300A;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_NATIVE_WINDOW: types::EGLenum = 0x300B;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_PARAMETER: types::EGLenum = 0x300C;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_STATE_KHR: types::EGLenum = 0x321C;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_STREAM_KHR: types::EGLenum = 0x321B;
#[allow(dead_code, non_upper_case_globals)] pub const BAD_SURFACE: types::EGLenum = 0x300D;
#[allow(dead_code, non_upper_case_globals)] pub const BIND_TO_TEXTURE_RGB: types::EGLenum = 0x3039;
#[allow(dead_code, non_upper_case_globals)] pub const BIND_TO_TEXTURE_RGBA: types::EGLenum = 0x303A;
#[allow(dead_code, non_upper_case_globals)] pub const BLUE_SIZE: types::EGLenum = 0x3022;
#[allow(dead_code, non_upper_case_globals)] pub const BUFFER_AGE_EXT: types::EGLenum = 0x313D;
#[allow(dead_code, non_upper_case_globals)] pub const BUFFER_DESTROYED: types::EGLenum = 0x3095;
#[allow(dead_code, non_upper_case_globals)] pub const BUFFER_PRESERVED: types::EGLenum = 0x3094;
#[allow(dead_code, non_upper_case_globals)] pub const BUFFER_SIZE: types::EGLenum = 0x3020;
#[allow(dead_code, non_upper_case_globals)] pub const CLIENT_APIS: types::EGLenum = 0x308D;
#[allow(dead_code, non_upper_case_globals)] pub const CL_EVENT_HANDLE: types::EGLenum = 0x309C;
#[allow(dead_code, non_upper_case_globals)] pub const COLORSPACE: types::EGLenum = 0x3087;
#[allow(dead_code, non_upper_case_globals)] pub const COLORSPACE_LINEAR: types::EGLenum = 0x308A;
#[allow(dead_code, non_upper_case_globals)] pub const COLORSPACE_sRGB: types::EGLenum = 0x3089;
#[allow(dead_code, non_upper_case_globals)] pub const COLOR_BUFFER_TYPE: types::EGLenum = 0x303F;
#[allow(dead_code, non_upper_case_globals)] pub const COLOR_COMPONENT_TYPE_EXT: types::EGLenum = 0x3339;
#[allow(dead_code, non_upper_case_globals)] pub const COLOR_COMPONENT_TYPE_FIXED_EXT: types::EGLenum = 0x333A;
#[allow(dead_code, non_upper_case_globals)] pub const COLOR_COMPONENT_TYPE_FLOAT_EXT: types::EGLenum = 0x333B;
#[allow(dead_code, non_upper_case_globals)] pub const CONDITION_SATISFIED: types::EGLenum = 0x30F6;
#[allow(dead_code, non_upper_case_globals)] pub const CONFIG_CAVEAT: types::EGLenum = 0x3027;
#[allow(dead_code, non_upper_case_globals)] pub const CONFIG_ID: types::EGLenum = 0x3028;
#[allow(dead_code, non_upper_case_globals)] pub const CONFORMANT: types::EGLenum = 0x3042;
#[allow(dead_code, non_upper_case_globals)] pub const CONSUMER_FRAME_KHR: types::EGLenum = 0x3213;
#[allow(dead_code, non_upper_case_globals)] pub const CONSUMER_LATENCY_USEC_KHR: types::EGLenum = 0x3210;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_CLIENT_TYPE: types::EGLenum = 0x3097;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_CLIENT_VERSION: types::EGLenum = 0x3098;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_FLAGS_KHR: types::EGLenum = 0x30FC;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_LOST: types::EGLenum = 0x300E;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_MAJOR_VERSION: types::EGLenum = 0x3098;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_MAJOR_VERSION_KHR: types::EGLenum = 0x3098;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_MINOR_VERSION: types::EGLenum = 0x30FB;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_MINOR_VERSION_KHR: types::EGLenum = 0x30FB;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT: types::EGLenum = 0x00000002;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT_KHR: types::EGLenum = 0x00000002;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_CORE_PROFILE_BIT: types::EGLenum = 0x00000001;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_CORE_PROFILE_BIT_KHR: types::EGLenum = 0x00000001;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_DEBUG: types::EGLenum = 0x31B0;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_DEBUG_BIT_KHR: types::EGLenum = 0x00000001;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_FORWARD_COMPATIBLE: types::EGLenum = 0x31B1;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR: types::EGLenum = 0x00000002;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_NO_ERROR_KHR: types::EGLenum = 0x31B3;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_PROFILE_MASK: types::EGLenum = 0x30FD;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_PROFILE_MASK_KHR: types::EGLenum = 0x30FD;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY: types::EGLenum = 0x31BD;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: types::EGLenum = 0x3138;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR: types::EGLenum = 0x31BD;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_ROBUST_ACCESS: types::EGLenum = 0x31B2;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR: types::EGLenum = 0x00000004;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_OPENGL_ROBUST_ACCESS_EXT: types::EGLenum = 0x30BF;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_PRIORITY_HIGH_IMG: types::EGLenum = 0x3101;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_PRIORITY_LEVEL_IMG: types::EGLenum = 0x3100;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_PRIORITY_LOW_IMG: types::EGLenum = 0x3103;
#[allow(dead_code, non_upper_case_globals)] pub const CONTEXT_PRIORITY_MEDIUM_IMG: types::EGLenum = 0x3102;
#[allow(dead_code, non_upper_case_globals)] pub const CORE_NATIVE_ENGINE: types::EGLenum = 0x305B;
#[allow(dead_code, non_upper_case_globals)] pub const DEFAULT_DISPLAY: types::EGLNativeDisplayType = 0 as types::EGLNativeDisplayType;
#[allow(dead_code, non_upper_case_globals)] pub const DEPTH_SIZE: types::EGLenum = 0x3025;
#[allow(dead_code, non_upper_case_globals)] pub const DEVICE_EXT: types::EGLenum = 0x322C;
#[allow(dead_code, non_upper_case_globals)] pub const DISPLAY_SCALING: types::EGLenum = 10000;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE0_FD_EXT: types::EGLenum = 0x3272;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE0_MODIFIER_HI_EXT: types::EGLenum = 0x3444;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE0_MODIFIER_LO_EXT: types::EGLenum = 0x3443;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE0_OFFSET_EXT: types::EGLenum = 0x3273;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE0_PITCH_EXT: types::EGLenum = 0x3274;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE1_FD_EXT: types::EGLenum = 0x3275;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE1_MODIFIER_HI_EXT: types::EGLenum = 0x3446;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE1_MODIFIER_LO_EXT: types::EGLenum = 0x3445;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE1_OFFSET_EXT: types::EGLenum = 0x3276;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE1_PITCH_EXT: types::EGLenum = 0x3277;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE2_FD_EXT: types::EGLenum = 0x3278;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE2_MODIFIER_HI_EXT: types::EGLenum = 0x3448;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE2_MODIFIER_LO_EXT: types::EGLenum = 0x3447;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE2_OFFSET_EXT: types::EGLenum = 0x3279;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE2_PITCH_EXT: types::EGLenum = 0x327A;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE3_FD_EXT: types::EGLenum = 0x3440;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE3_MODIFIER_HI_EXT: types::EGLenum = 0x344A;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE3_MODIFIER_LO_EXT: types::EGLenum = 0x3449;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE3_OFFSET_EXT: types::EGLenum = 0x3441;
#[allow(dead_code, non_upper_case_globals)] pub const DMA_BUF_PLANE3_PITCH_EXT: types::EGLenum = 0x3442;
#[allow(dead_code, non_upper_case_globals)] pub const DONT_CARE: types::EGLint = -1 as types::EGLint;
#[allow(dead_code, non_upper_case_globals)] pub const DRAW: types::EGLenum = 0x3059;
#[allow(dead_code, non_upper_case_globals)] pub const DRM_DEVICE_FILE_EXT: types::EGLenum = 0x3233;
#[allow(dead_code, non_upper_case_globals)] pub const DRM_MASTER_FD_EXT: types::EGLenum = 0x333C;
#[allow(dead_code, non_upper_case_globals)] pub const EXTENSIONS: types::EGLenum = 0x3055;
#[allow(dead_code, non_upper_case_globals)] pub const FALSE: types::EGLBoolean = 0;
#[allow(dead_code, non_upper_case_globals)] pub const FOREVER: types::EGLuint64KHR = 0xFFFFFFFFFFFFFFFF;
#[allow(dead_code, non_upper_case_globals)] pub const GL_COLORSPACE: types::EGLenum = 0x309D;
#[allow(dead_code, non_upper_case_globals)] pub const GL_COLORSPACE_LINEAR: types::EGLenum = 0x308A;
#[allow(dead_code, non_upper_case_globals)] pub const GL_COLORSPACE_SRGB: types::EGLenum = 0x3089;
#[allow(dead_code, non_upper_case_globals)] pub const GL_RENDERBUFFER: types::EGLenum = 0x30B9;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_2D: types::EGLenum = 0x30B1;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_3D: types::EGLenum = 0x30B2;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_NEGATIVE_X: types::EGLenum = 0x30B4;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_NEGATIVE_Y: types::EGLenum = 0x30B6;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_NEGATIVE_Z: types::EGLenum = 0x30B8;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_POSITIVE_X: types::EGLenum = 0x30B3;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_POSITIVE_Y: types::EGLenum = 0x30B5;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_CUBE_MAP_POSITIVE_Z: types::EGLenum = 0x30B7;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_LEVEL: types::EGLenum = 0x30BC;
#[allow(dead_code, non_upper_case_globals)] pub const GL_TEXTURE_ZOFFSET: types::EGLenum = 0x30BD;
#[allow(dead_code, non_upper_case_globals)] pub const GREEN_SIZE: types::EGLenum = 0x3023;
#[allow(dead_code, non_upper_case_globals)] pub const HEIGHT: types::EGLenum = 0x3056;
#[allow(dead_code, non_upper_case_globals)] pub const HORIZONTAL_RESOLUTION: types::EGLenum = 0x3090;
#[allow(dead_code, non_upper_case_globals)] pub const IMAGE_PRESERVED: types::EGLenum = 0x30D2;
#[allow(dead_code, non_upper_case_globals)] pub const IMAGE_PRESERVED_KHR: types::EGLenum = 0x30D2;
#[allow(dead_code, non_upper_case_globals)] pub const ITU_REC2020_EXT: types::EGLenum = 0x3281;
#[allow(dead_code, non_upper_case_globals)] pub const ITU_REC601_EXT: types::EGLenum = 0x327F;
#[allow(dead_code, non_upper_case_globals)] pub const ITU_REC709_EXT: types::EGLenum = 0x3280;
#[allow(dead_code, non_upper_case_globals)] pub const LARGEST_PBUFFER: types::EGLenum = 0x3058;
#[allow(dead_code, non_upper_case_globals)] pub const LEVEL: types::EGLenum = 0x3029;
#[allow(dead_code, non_upper_case_globals)] pub const LINUX_DMA_BUF_EXT: types::EGLenum = 0x3270;
#[allow(dead_code, non_upper_case_globals)] pub const LINUX_DRM_FOURCC_EXT: types::EGLenum = 0x3271;
#[allow(dead_code, non_upper_case_globals)] pub const LOSE_CONTEXT_ON_RESET: types::EGLenum = 0x31BF;
#[allow(dead_code, non_upper_case_globals)] pub const LOSE_CONTEXT_ON_RESET_EXT: types::EGLenum = 0x31BF;
#[allow(dead_code, non_upper_case_globals)] pub const LOSE_CONTEXT_ON_RESET_KHR: types::EGLenum = 0x31BF;
#[allow(dead_code, non_upper_case_globals)] pub const LUMINANCE_BUFFER: types::EGLenum = 0x308F;
#[allow(dead_code, non_upper_case_globals)] pub const LUMINANCE_SIZE: types::EGLenum = 0x303D;
#[allow(dead_code, non_upper_case_globals)] pub const MATCH_NATIVE_PIXMAP: types::EGLenum = 0x3041;
#[allow(dead_code, non_upper_case_globals)] pub const MAX_PBUFFER_HEIGHT: types::EGLenum = 0x302A;
#[allow(dead_code, non_upper_case_globals)] pub const MAX_PBUFFER_PIXELS: types::EGLenum = 0x302B;
#[allow(dead_code, non_upper_case_globals)] pub const MAX_PBUFFER_WIDTH: types::EGLenum = 0x302C;
#[allow(dead_code, non_upper_case_globals)] pub const MAX_SWAP_INTERVAL: types::EGLenum = 0x303C;
#[allow(dead_code, non_upper_case_globals)] pub const MIN_SWAP_INTERVAL: types::EGLenum = 0x303B;
#[allow(dead_code, non_upper_case_globals)] pub const MIPMAP_LEVEL: types::EGLenum = 0x3083;
#[allow(dead_code, non_upper_case_globals)] pub const MIPMAP_TEXTURE: types::EGLenum = 0x3082;
#[allow(dead_code, non_upper_case_globals)] pub const MULTISAMPLE_RESOLVE: types::EGLenum = 0x3099;
#[allow(dead_code, non_upper_case_globals)] pub const MULTISAMPLE_RESOLVE_BOX: types::EGLenum = 0x309B;
#[allow(dead_code, non_upper_case_globals)] pub const MULTISAMPLE_RESOLVE_BOX_BIT: types::EGLenum = 0x0200;
#[allow(dead_code, non_upper_case_globals)] pub const MULTISAMPLE_RESOLVE_DEFAULT: types::EGLenum = 0x309A;
#[allow(dead_code, non_upper_case_globals)] pub const NATIVE_RENDERABLE: types::EGLenum = 0x302D;
#[allow(dead_code, non_upper_case_globals)] pub const NATIVE_VISUAL_ID: types::EGLenum = 0x302E;
#[allow(dead_code, non_upper_case_globals)] pub const NATIVE_VISUAL_TYPE: types::EGLenum = 0x302F;
#[allow(dead_code, non_upper_case_globals)] pub const NONE: types::EGLenum = 0x3038;
#[allow(dead_code, non_upper_case_globals)] pub const NON_CONFORMANT_CONFIG: types::EGLenum = 0x3051;
#[allow(dead_code, non_upper_case_globals)] pub const NOT_INITIALIZED: types::EGLenum = 0x3001;
#[allow(dead_code, non_upper_case_globals)] pub const NO_CONFIG_KHR: types::EGLConfig = 0 as types::EGLConfig;
#[allow(dead_code, non_upper_case_globals)] pub const NO_CONTEXT: types::EGLContext = 0 as types::EGLContext;
#[allow(dead_code, non_upper_case_globals)] pub const NO_DEVICE_EXT: types::EGLDeviceEXT = 0 as types::EGLDeviceEXT;
#[allow(dead_code, non_upper_case_globals)] pub const NO_DISPLAY: types::EGLDisplay = 0 as types::EGLDisplay;
#[allow(dead_code, non_upper_case_globals)] pub const NO_IMAGE: types::EGLImage = 0 as types::EGLImage;
#[allow(dead_code, non_upper_case_globals)] pub const NO_IMAGE_KHR: types::EGLImageKHR = 0 as types::EGLImageKHR;
#[allow(dead_code, non_upper_case_globals)] pub const NO_NATIVE_FENCE_FD_ANDROID: types::EGLint = -1;
#[allow(dead_code, non_upper_case_globals)] pub const NO_RESET_NOTIFICATION: types::EGLenum = 0x31BE;
#[allow(dead_code, non_upper_case_globals)] pub const NO_RESET_NOTIFICATION_EXT: types::EGLenum = 0x31BE;
#[allow(dead_code, non_upper_case_globals)] pub const NO_RESET_NOTIFICATION_KHR: types::EGLenum = 0x31BE;
#[allow(dead_code, non_upper_case_globals)] pub const NO_STREAM_KHR: types::EGLStreamKHR = 0 as types::EGLStreamKHR;
#[allow(dead_code, non_upper_case_globals)] pub const NO_SURFACE: types::EGLSurface = 0 as types::EGLSurface;
#[allow(dead_code, non_upper_case_globals)] pub const NO_SYNC: types::EGLSync = 0 as types::EGLSync;
#[allow(dead_code, non_upper_case_globals)] pub const NO_TEXTURE: types::EGLenum = 0x305C;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_API: types::EGLenum = 0x30A2;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_BIT: types::EGLenum = 0x0008;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_ES2_BIT: types::EGLenum = 0x0004;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_ES3_BIT: types::EGLenum = 0x00000040;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_ES3_BIT_KHR: types::EGLenum = 0x00000040;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_ES_API: types::EGLenum = 0x30A0;
#[allow(dead_code, non_upper_case_globals)] pub const OPENGL_ES_BIT: types::EGLenum = 0x0001;
#[allow(dead_code, non_upper_case_globals)] pub const OPENVG_API: types::EGLenum = 0x30A1;
#[allow(dead_code, non_upper_case_globals)] pub const OPENVG_BIT: types::EGLenum = 0x0002;
#[allow(dead_code, non_upper_case_globals)] pub const OPENVG_IMAGE: types::EGLenum = 0x3096;
#[allow(dead_code, non_upper_case_globals)] pub const PBUFFER_BIT: types::EGLenum = 0x0001;
#[allow(dead_code, non_upper_case_globals)] pub const PIXEL_ASPECT_RATIO: types::EGLenum = 0x3092;
#[allow(dead_code, non_upper_case_globals)] pub const PIXMAP_BIT: types::EGLenum = 0x0002;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_DEVICE_EXT: types::EGLenum = 0x313F;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_GBM_KHR: types::EGLenum = 0x31D7;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_GBM_MESA: types::EGLenum = 0x31D7;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_SURFACELESS_MESA: types::EGLenum = 0x31DD;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_WAYLAND_EXT: types::EGLenum = 0x31D8;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_WAYLAND_KHR: types::EGLenum = 0x31D8;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_X11_EXT: types::EGLenum = 0x31D5;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_X11_KHR: types::EGLenum = 0x31D5;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_X11_SCREEN_EXT: types::EGLenum = 0x31D6;
#[allow(dead_code, non_upper_case_globals)] pub const PLATFORM_X11_SCREEN_KHR: types::EGLenum = 0x31D6;
#[allow(dead_code, non_upper_case_globals)] pub const PRODUCER_FRAME_KHR: types::EGLenum = 0x3212;
#[allow(dead_code, non_upper_case_globals)] pub const READ: types::EGLenum = 0x305A;
#[allow(dead_code, non_upper_case_globals)] pub const RED_SIZE: types::EGLenum = 0x3024;
#[allow(dead_code, non_upper_case_globals)] pub const RENDERABLE_TYPE: types::EGLenum = 0x3040;
#[allow(dead_code, non_upper_case_globals)] pub const RENDER_BUFFER: types::EGLenum = 0x3086;
#[allow(dead_code, non_upper_case_globals)] pub const RGB_BUFFER: types::EGLenum = 0x308E;
#[allow(dead_code, non_upper_case_globals)] pub const SAMPLES: types::EGLenum = 0x3031;
#[allow(dead_code, non_upper_case_globals)] pub const SAMPLE_BUFFERS: types::EGLenum = 0x3032;
#[allow(dead_code, non_upper_case_globals)] pub const SAMPLE_RANGE_HINT_EXT: types::EGLenum = 0x327C;
#[allow(dead_code, non_upper_case_globals)] pub const SIGNALED: types::EGLenum = 0x30F2;
#[allow(dead_code, non_upper_case_globals)] pub const SINGLE_BUFFER: types::EGLenum = 0x3085;
#[allow(dead_code, non_upper_case_globals)] pub const SLOW_CONFIG: types::EGLenum = 0x3050;
#[allow(dead_code, non_upper_case_globals)] pub const STENCIL_SIZE: types::EGLenum = 0x3026;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_BIT_KHR: types::EGLenum = 0x0800;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_CONNECTING_KHR: types::EGLenum = 0x3216;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_CREATED_KHR: types::EGLenum = 0x3215;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_DISCONNECTED_KHR: types::EGLenum = 0x321A;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_EMPTY_KHR: types::EGLenum = 0x3217;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_KHR: types::EGLenum = 0x3214;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_NEW_FRAME_AVAILABLE_KHR: types::EGLenum = 0x3218;
#[allow(dead_code, non_upper_case_globals)] pub const STREAM_STATE_OLD_FRAME_AVAILABLE_KHR: types::EGLenum = 0x3219;
#[allow(dead_code, non_upper_case_globals)] pub const SUCCESS: types::EGLenum = 0x3000;
#[allow(dead_code, non_upper_case_globals)] pub const SURFACE_TYPE: types::EGLenum = 0x3033;
#[allow(dead_code, non_upper_case_globals)] pub const SWAP_BEHAVIOR: types::EGLenum = 0x3093;
#[allow(dead_code, non_upper_case_globals)] pub const SWAP_BEHAVIOR_PRESERVED_BIT: types::EGLenum = 0x0400;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_CL_EVENT: types::EGLenum = 0x30FE;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_CL_EVENT_COMPLETE: types::EGLenum = 0x30FF;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_CONDITION: types::EGLenum = 0x30F8;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_CONDITION_KHR: types::EGLenum = 0x30F8;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_FENCE: types::EGLenum = 0x30F9;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_FENCE_KHR: types::EGLenum = 0x30F9;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_FLUSH_COMMANDS_BIT: types::EGLenum = 0x0001;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_NATIVE_FENCE_ANDROID: types::EGLenum = 0x3144;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_NATIVE_FENCE_FD_ANDROID: types::EGLenum = 0x3145;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_NATIVE_FENCE_SIGNALED_ANDROID: types::EGLenum = 0x3146;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_PRIOR_COMMANDS_COMPLETE: types::EGLenum = 0x30F0;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_PRIOR_COMMANDS_COMPLETE_KHR: types::EGLenum = 0x30F0;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_STATUS: types::EGLenum = 0x30F1;
#[allow(dead_code, non_upper_case_globals)] pub const SYNC_TYPE: types::EGLenum = 0x30F7;
#[allow(dead_code, non_upper_case_globals)] pub const TEXTURE_2D: types::EGLenum = 0x305F;
#[allow(dead_code, non_upper_case_globals)] pub const TEXTURE_FORMAT: types::EGLenum = 0x3080;
#[allow(dead_code, non_upper_case_globals)] pub const TEXTURE_RGB: types::EGLenum = 0x305D;
#[allow(dead_code, non_upper_case_globals)] pub const TEXTURE_RGBA: types::EGLenum = 0x305E;
#[allow(dead_code, non_upper_case_globals)] pub const TEXTURE_TARGET: types::EGLenum = 0x3081;
#[allow(dead_code, non_upper_case_globals)] pub const TIMEOUT_EXPIRED: types::EGLenum = 0x30F5;
#[allow(dead_code, non_upper_case_globals)] pub const TRANSPARENT_BLUE_VALUE: types::EGLenum = 0x3035;
#[allow(dead_code, non_upper_case_globals)] pub const TRANSPARENT_GREEN_VALUE: types::EGLenum = 0x3036;
#[allow(dead_code, non_upper_case_globals)] pub const TRANSPARENT_RED_VALUE: types::EGLenum = 0x3037;
#[allow(dead_code, non_upper_case_globals)] pub const TRANSPARENT_RGB: types::EGLenum = 0x3052;
#[allow(dead_code, non_upper_case_globals)] pub const TRANSPARENT_TYPE: types::EGLenum = 0x3034;
#[allow(dead_code, non_upper_case_globals)] pub const TRUE: types::EGLBoolean = 1;
#[allow(dead_code, non_upper_case_globals)] pub const UNKNOWN: types::EGLint = -1 as types::EGLint;
#[allow(dead_code, non_upper_case_globals)] pub const UNSIGNALED: types::EGLenum = 0x30F3;
#[allow(dead_code, non_upper_case_globals)] pub const VENDOR: types::EGLenum = 0x3053;
#[allow(dead_code, non_upper_case_globals)] pub const VERSION: types::EGLenum = 0x3054;
#[allow(dead_code, non_upper_case_globals)] pub const VERTICAL_RESOLUTION: types::EGLenum = 0x3091;
#[allow(dead_code, non_upper_case_globals)] pub const VG_ALPHA_FORMAT: types::EGLenum = 0x3088;
#[allow(dead_code, non_upper_case_globals)] pub const VG_ALPHA_FORMAT_NONPRE: types::EGLenum = 0x308B;
#[allow(dead_code, non_upper_case_globals)] pub const VG_ALPHA_FORMAT_PRE: types::EGLenum = 0x308C;
#[allow(dead_code, non_upper_case_globals)] pub const VG_ALPHA_FORMAT_PRE_BIT: types::EGLenum = 0x0040;
#[allow(dead_code, non_upper_case_globals)] pub const VG_COLORSPACE: types::EGLenum = 0x3087;
#[allow(dead_code, non_upper_case_globals)] pub const VG_COLORSPACE_LINEAR: types::EGLenum = 0x308A;
#[allow(dead_code, non_upper_case_globals)] pub const VG_COLORSPACE_LINEAR_BIT: types::EGLenum = 0x0020;
#[allow(dead_code, non_upper_case_globals)] pub const VG_COLORSPACE_sRGB: types::EGLenum = 0x3089;
#[allow(dead_code, non_upper_case_globals)] pub const WIDTH: types::EGLenum = 0x3057;
#[allow(dead_code, non_upper_case_globals)] pub const WINDOW_BIT: types::EGLenum = 0x0004;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_CHROMA_HORIZONTAL_SITING_HINT_EXT: types::EGLenum = 0x327D;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_CHROMA_SITING_0_5_EXT: types::EGLenum = 0x3285;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_CHROMA_SITING_0_EXT: types::EGLenum = 0x3284;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_CHROMA_VERTICAL_SITING_HINT_EXT: types::EGLenum = 0x327E;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_COLOR_SPACE_HINT_EXT: types::EGLenum = 0x327B;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_FULL_RANGE_EXT: types::EGLenum = 0x3282;
#[allow(dead_code, non_upper_case_globals)] pub const YUV_NARROW_RANGE_EXT: types::EGLenum = 0x3283;
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn BindAPI(api: types::EGLenum) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLenum) -> types::EGLBoolean>(storage::BindAPI.f)(api) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn BindTexImage(dpy: types::EGLDisplay, surface: types::EGLSurface, buffer: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLint) -> types::EGLBoolean>(storage::BindTexImage.f)(dpy, surface, buffer) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ChooseConfig(dpy: types::EGLDisplay, attrib_list: *const types::EGLint, configs: *mut types::EGLConfig, config_size: types::EGLint, num_config: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, *const types::EGLint, *mut types::EGLConfig, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::ChooseConfig.f)(dpy, attrib_list, configs, config_size, num_config) }
/// Fallbacks: ClientWaitSyncKHR
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ClientWaitSync(dpy: types::EGLDisplay, sync: types::EGLSync, flags: types::EGLint, timeout: types::EGLTime) -> types::EGLint { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSync, types::EGLint, types::EGLTime) -> types::EGLint>(storage::ClientWaitSync.f)(dpy, sync, flags, timeout) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ClientWaitSyncKHR(dpy: types::EGLDisplay, sync: types::EGLSyncKHR, flags: types::EGLint, timeout: types::EGLTimeKHR) -> types::EGLint { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSyncKHR, types::EGLint, types::EGLTimeKHR) -> types::EGLint>(storage::ClientWaitSyncKHR.f)(dpy, sync, flags, timeout) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CopyBuffers(dpy: types::EGLDisplay, surface: types::EGLSurface, target: types::EGLNativePixmapType) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLNativePixmapType) -> types::EGLBoolean>(storage::CopyBuffers.f)(dpy, surface, target) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateContext(dpy: types::EGLDisplay, config: types::EGLConfig, share_context: types::EGLContext, attrib_list: *const types::EGLint) -> types::EGLContext { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, types::EGLContext, *const types::EGLint) -> types::EGLContext>(storage::CreateContext.f)(dpy, config, share_context, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateImage(dpy: types::EGLDisplay, ctx: types::EGLContext, target: types::EGLenum, buffer: types::EGLClientBuffer, attrib_list: *const types::EGLAttrib) -> types::EGLImage { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLContext, types::EGLenum, types::EGLClientBuffer, *const types::EGLAttrib) -> types::EGLImage>(storage::CreateImage.f)(dpy, ctx, target, buffer, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateImageKHR(dpy: types::EGLDisplay, ctx: types::EGLContext, target: types::EGLenum, buffer: types::EGLClientBuffer, attrib_list: *const types::EGLint) -> types::EGLImageKHR { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLContext, types::EGLenum, types::EGLClientBuffer, *const types::EGLint) -> types::EGLImageKHR>(storage::CreateImageKHR.f)(dpy, ctx, target, buffer, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePbufferFromClientBuffer(dpy: types::EGLDisplay, buftype: types::EGLenum, buffer: types::EGLClientBuffer, config: types::EGLConfig, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLenum, types::EGLClientBuffer, types::EGLConfig, *const types::EGLint) -> types::EGLSurface>(storage::CreatePbufferFromClientBuffer.f)(dpy, buftype, buffer, config, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePbufferSurface(dpy: types::EGLDisplay, config: types::EGLConfig, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, *const types::EGLint) -> types::EGLSurface>(storage::CreatePbufferSurface.f)(dpy, config, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePixmapSurface(dpy: types::EGLDisplay, config: types::EGLConfig, pixmap: types::EGLNativePixmapType, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, types::EGLNativePixmapType, *const types::EGLint) -> types::EGLSurface>(storage::CreatePixmapSurface.f)(dpy, config, pixmap, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePlatformPixmapSurface(dpy: types::EGLDisplay, config: types::EGLConfig, native_pixmap: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLAttrib) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, *mut __gl_imports::raw::c_void, *const types::EGLAttrib) -> types::EGLSurface>(storage::CreatePlatformPixmapSurface.f)(dpy, config, native_pixmap, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePlatformPixmapSurfaceEXT(dpy: types::EGLDisplay, config: types::EGLConfig, native_pixmap: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, *mut __gl_imports::raw::c_void, *const types::EGLint) -> types::EGLSurface>(storage::CreatePlatformPixmapSurfaceEXT.f)(dpy, config, native_pixmap, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePlatformWindowSurface(dpy: types::EGLDisplay, config: types::EGLConfig, native_window: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLAttrib) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, *mut __gl_imports::raw::c_void, *const types::EGLAttrib) -> types::EGLSurface>(storage::CreatePlatformWindowSurface.f)(dpy, config, native_window, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreatePlatformWindowSurfaceEXT(dpy: types::EGLDisplay, config: types::EGLConfig, native_window: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, *mut __gl_imports::raw::c_void, *const types::EGLint) -> types::EGLSurface>(storage::CreatePlatformWindowSurfaceEXT.f)(dpy, config, native_window, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateStreamKHR(dpy: types::EGLDisplay, attrib_list: *const types::EGLint) -> types::EGLStreamKHR { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, *const types::EGLint) -> types::EGLStreamKHR>(storage::CreateStreamKHR.f)(dpy, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateStreamProducerSurfaceKHR(dpy: types::EGLDisplay, config: types::EGLConfig, stream: types::EGLStreamKHR, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, types::EGLStreamKHR, *const types::EGLint) -> types::EGLSurface>(storage::CreateStreamProducerSurfaceKHR.f)(dpy, config, stream, attrib_list) }
/// Fallbacks: CreateSync64KHR
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateSync(dpy: types::EGLDisplay, type_: types::EGLenum, attrib_list: *const types::EGLAttrib) -> types::EGLSync { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLenum, *const types::EGLAttrib) -> types::EGLSync>(storage::CreateSync.f)(dpy, type_, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateSyncKHR(dpy: types::EGLDisplay, type_: types::EGLenum, attrib_list: *const types::EGLint) -> types::EGLSyncKHR { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLenum, *const types::EGLint) -> types::EGLSyncKHR>(storage::CreateSyncKHR.f)(dpy, type_, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn CreateWindowSurface(dpy: types::EGLDisplay, config: types::EGLConfig, win: types::EGLNativeWindowType, attrib_list: *const types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, types::EGLNativeWindowType, *const types::EGLint) -> types::EGLSurface>(storage::CreateWindowSurface.f)(dpy, config, win, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroyContext(dpy: types::EGLDisplay, ctx: types::EGLContext) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLContext) -> types::EGLBoolean>(storage::DestroyContext.f)(dpy, ctx) }
/// Fallbacks: DestroyImageKHR
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroyImage(dpy: types::EGLDisplay, image: types::EGLImage) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLImage) -> types::EGLBoolean>(storage::DestroyImage.f)(dpy, image) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroyImageKHR(dpy: types::EGLDisplay, image: types::EGLImageKHR) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLImageKHR) -> types::EGLBoolean>(storage::DestroyImageKHR.f)(dpy, image) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroyStreamKHR(dpy: types::EGLDisplay, stream: types::EGLStreamKHR) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLStreamKHR) -> types::EGLBoolean>(storage::DestroyStreamKHR.f)(dpy, stream) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroySurface(dpy: types::EGLDisplay, surface: types::EGLSurface) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface) -> types::EGLBoolean>(storage::DestroySurface.f)(dpy, surface) }
/// Fallbacks: DestroySyncKHR
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroySync(dpy: types::EGLDisplay, sync: types::EGLSync) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSync) -> types::EGLBoolean>(storage::DestroySync.f)(dpy, sync) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DestroySyncKHR(dpy: types::EGLDisplay, sync: types::EGLSyncKHR) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSyncKHR) -> types::EGLBoolean>(storage::DestroySyncKHR.f)(dpy, sync) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn DupNativeFenceFDANDROID(dpy: types::EGLDisplay, sync: types::EGLSyncKHR) -> types::EGLint { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSyncKHR) -> types::EGLint>(storage::DupNativeFenceFDANDROID.f)(dpy, sync) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ExportDMABUFImageMESA(dpy: types::EGLDisplay, image: types::EGLImageKHR, fds: *mut __gl_imports::raw::c_int, strides: *mut types::EGLint, offsets: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLImageKHR, *mut __gl_imports::raw::c_int, *mut types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::ExportDMABUFImageMESA.f)(dpy, image, fds, strides, offsets) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ExportDMABUFImageQueryMESA(dpy: types::EGLDisplay, image: types::EGLImageKHR, fourcc: *mut __gl_imports::raw::c_int, num_planes: *mut __gl_imports::raw::c_int, modifiers: *mut types::EGLuint64KHR) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLImageKHR, *mut __gl_imports::raw::c_int, *mut __gl_imports::raw::c_int, *mut types::EGLuint64KHR) -> types::EGLBoolean>(storage::ExportDMABUFImageQueryMESA.f)(dpy, image, fourcc, num_planes, modifiers) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetConfigAttrib(dpy: types::EGLDisplay, config: types::EGLConfig, attribute: types::EGLint, value: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLConfig, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::GetConfigAttrib.f)(dpy, config, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetConfigs(dpy: types::EGLDisplay, configs: *mut types::EGLConfig, config_size: types::EGLint, num_config: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, *mut types::EGLConfig, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::GetConfigs.f)(dpy, configs, config_size, num_config) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetCurrentContext() -> types::EGLContext { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLContext>(storage::GetCurrentContext.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetCurrentDisplay() -> types::EGLDisplay { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLDisplay>(storage::GetCurrentDisplay.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetCurrentSurface(readdraw: types::EGLint) -> types::EGLSurface { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLint) -> types::EGLSurface>(storage::GetCurrentSurface.f)(readdraw) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetDisplay(display_id: types::EGLNativeDisplayType) -> types::EGLDisplay { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLNativeDisplayType) -> types::EGLDisplay>(storage::GetDisplay.f)(display_id) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetError() -> types::EGLint { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLint>(storage::GetError.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetPlatformDisplay(platform: types::EGLenum, native_display: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLAttrib) -> types::EGLDisplay { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLenum, *mut __gl_imports::raw::c_void, *const types::EGLAttrib) -> types::EGLDisplay>(storage::GetPlatformDisplay.f)(platform, native_display, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetPlatformDisplayEXT(platform: types::EGLenum, native_display: *mut __gl_imports::raw::c_void, attrib_list: *const types::EGLint) -> types::EGLDisplay { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLenum, *mut __gl_imports::raw::c_void, *const types::EGLint) -> types::EGLDisplay>(storage::GetPlatformDisplayEXT.f)(platform, native_display, attrib_list) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetProcAddress(procname: *const __gl_imports::raw::c_char) -> types::__eglMustCastToProperFunctionPointerType { __gl_imports::mem::transmute::<_, extern "system" fn(*const __gl_imports::raw::c_char) -> types::__eglMustCastToProperFunctionPointerType>(storage::GetProcAddress.f)(procname) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetSyncAttrib(dpy: types::EGLDisplay, sync: types::EGLSync, attribute: types::EGLint, value: *mut types::EGLAttrib) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSync, types::EGLint, *mut types::EGLAttrib) -> types::EGLBoolean>(storage::GetSyncAttrib.f)(dpy, sync, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn GetSyncAttribKHR(dpy: types::EGLDisplay, sync: types::EGLSyncKHR, attribute: types::EGLint, value: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSyncKHR, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::GetSyncAttribKHR.f)(dpy, sync, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn Initialize(dpy: types::EGLDisplay, major: *mut types::EGLint, minor: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, *mut types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::Initialize.f)(dpy, major, minor) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn MakeCurrent(dpy: types::EGLDisplay, draw: types::EGLSurface, read: types::EGLSurface, ctx: types::EGLContext) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLSurface, types::EGLContext) -> types::EGLBoolean>(storage::MakeCurrent.f)(dpy, draw, read, ctx) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryAPI() -> types::EGLenum { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLenum>(storage::QueryAPI.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryContext(dpy: types::EGLDisplay, ctx: types::EGLContext, attribute: types::EGLint, value: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLContext, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::QueryContext.f)(dpy, ctx, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDeviceAttribEXT(device: types::EGLDeviceEXT, attribute: types::EGLint, value: *mut types::EGLAttrib) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDeviceEXT, types::EGLint, *mut types::EGLAttrib) -> types::EGLBoolean>(storage::QueryDeviceAttribEXT.f)(device, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDeviceStringEXT(device: types::EGLDeviceEXT, name: types::EGLint) -> *const __gl_imports::raw::c_char { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDeviceEXT, types::EGLint) -> *const __gl_imports::raw::c_char>(storage::QueryDeviceStringEXT.f)(device, name) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDevicesEXT(max_devices: types::EGLint, devices: *mut types::EGLDeviceEXT, num_devices: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLint, *mut types::EGLDeviceEXT, *mut types::EGLint) -> types::EGLBoolean>(storage::QueryDevicesEXT.f)(max_devices, devices, num_devices) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDisplayAttribEXT(dpy: types::EGLDisplay, attribute: types::EGLint, value: *mut types::EGLAttrib) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLint, *mut types::EGLAttrib) -> types::EGLBoolean>(storage::QueryDisplayAttribEXT.f)(dpy, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDmaBufFormatsEXT(dpy: types::EGLDisplay, max_formats: types::EGLint, formats: *mut types::EGLint, num_formats: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLint, *mut types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::QueryDmaBufFormatsEXT.f)(dpy, max_formats, formats, num_formats) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryDmaBufModifiersEXT(dpy: types::EGLDisplay, format: types::EGLint, max_modifiers: types::EGLint, modifiers: *mut types::EGLuint64KHR, external_only: *mut types::EGLBoolean, num_modifiers: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLint, types::EGLint, *mut types::EGLuint64KHR, *mut types::EGLBoolean, *mut types::EGLint) -> types::EGLBoolean>(storage::QueryDmaBufModifiersEXT.f)(dpy, format, max_modifiers, modifiers, external_only, num_modifiers) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryStreamKHR(dpy: types::EGLDisplay, stream: types::EGLStreamKHR, attribute: types::EGLenum, value: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLStreamKHR, types::EGLenum, *mut types::EGLint) -> types::EGLBoolean>(storage::QueryStreamKHR.f)(dpy, stream, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryStreamu64KHR(dpy: types::EGLDisplay, stream: types::EGLStreamKHR, attribute: types::EGLenum, value: *mut types::EGLuint64KHR) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLStreamKHR, types::EGLenum, *mut types::EGLuint64KHR) -> types::EGLBoolean>(storage::QueryStreamu64KHR.f)(dpy, stream, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QueryString(dpy: types::EGLDisplay, name: types::EGLint) -> *const __gl_imports::raw::c_char { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLint) -> *const __gl_imports::raw::c_char>(storage::QueryString.f)(dpy, name) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn QuerySurface(dpy: types::EGLDisplay, surface: types::EGLSurface, attribute: types::EGLint, value: *mut types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLint, *mut types::EGLint) -> types::EGLBoolean>(storage::QuerySurface.f)(dpy, surface, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ReleaseTexImage(dpy: types::EGLDisplay, surface: types::EGLSurface, buffer: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLint) -> types::EGLBoolean>(storage::ReleaseTexImage.f)(dpy, surface, buffer) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn ReleaseThread() -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLBoolean>(storage::ReleaseThread.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn StreamAttribKHR(dpy: types::EGLDisplay, stream: types::EGLStreamKHR, attribute: types::EGLenum, value: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLStreamKHR, types::EGLenum, types::EGLint) -> types::EGLBoolean>(storage::StreamAttribKHR.f)(dpy, stream, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn SurfaceAttrib(dpy: types::EGLDisplay, surface: types::EGLSurface, attribute: types::EGLint, value: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, types::EGLint, types::EGLint) -> types::EGLBoolean>(storage::SurfaceAttrib.f)(dpy, surface, attribute, value) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn SwapBuffers(dpy: types::EGLDisplay, surface: types::EGLSurface) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface) -> types::EGLBoolean>(storage::SwapBuffers.f)(dpy, surface) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn SwapBuffersWithDamageEXT(dpy: types::EGLDisplay, surface: types::EGLSurface, rects: *mut types::EGLint, n_rects: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, *mut types::EGLint, types::EGLint) -> types::EGLBoolean>(storage::SwapBuffersWithDamageEXT.f)(dpy, surface, rects, n_rects) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn SwapBuffersWithDamageKHR(dpy: types::EGLDisplay, surface: types::EGLSurface, rects: *mut types::EGLint, n_rects: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSurface, *mut types::EGLint, types::EGLint) -> types::EGLBoolean>(storage::SwapBuffersWithDamageKHR.f)(dpy, surface, rects, n_rects) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn SwapInterval(dpy: types::EGLDisplay, interval: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLint) -> types::EGLBoolean>(storage::SwapInterval.f)(dpy, interval) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn Terminate(dpy: types::EGLDisplay) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay) -> types::EGLBoolean>(storage::Terminate.f)(dpy) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn WaitClient() -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLBoolean>(storage::WaitClient.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn WaitGL() -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn() -> types::EGLBoolean>(storage::WaitGL.f)() }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn WaitNative(engine: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLint) -> types::EGLBoolean>(storage::WaitNative.f)(engine) }
#[allow(non_snake_case, unused_variables, dead_code)] #[inline]
            pub unsafe fn WaitSync(dpy: types::EGLDisplay, sync: types::EGLSync, flags: types::EGLint) -> types::EGLBoolean { __gl_imports::mem::transmute::<_, extern "system" fn(types::EGLDisplay, types::EGLSync, types::EGLint) -> types::EGLBoolean>(storage::WaitSync.f)(dpy, sync, flags) }

        #[allow(missing_copy_implementations)]
        pub struct FnPtr {
            /// The function pointer that will be used when calling the function.
            f: *const __gl_imports::raw::c_void,
            /// True if the pointer points to a real function, false if points to a `panic!` fn.
            is_loaded: bool,
        }

        impl FnPtr {
            /// Creates a `FnPtr` from a load attempt.
            pub fn new(ptr: *const __gl_imports::raw::c_void) -> FnPtr {
                if ptr.is_null() {
                    FnPtr { f: missing_fn_panic as *const __gl_imports::raw::c_void, is_loaded: false }
                } else {
                    FnPtr { f: ptr, is_loaded: true }
                }
            }
        }
    
mod storage {
            #![allow(non_snake_case)]
            #![allow(non_upper_case_globals)]
            use super::__gl_imports::raw;
            use super::FnPtr;
pub static mut BindAPI: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut BindTexImage: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ChooseConfig: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ClientWaitSync: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ClientWaitSyncKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CopyBuffers: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateContext: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateImage: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateImageKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePbufferFromClientBuffer: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePbufferSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePixmapSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePlatformPixmapSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePlatformPixmapSurfaceEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePlatformWindowSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreatePlatformWindowSurfaceEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateStreamKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateStreamProducerSurfaceKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateSync: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateSyncKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut CreateWindowSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroyContext: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroyImage: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroyImageKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroyStreamKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroySurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroySync: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DestroySyncKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut DupNativeFenceFDANDROID: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ExportDMABUFImageMESA: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ExportDMABUFImageQueryMESA: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetConfigAttrib: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetConfigs: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetCurrentContext: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetCurrentDisplay: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetCurrentSurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetDisplay: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetError: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetPlatformDisplay: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetPlatformDisplayEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetProcAddress: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetSyncAttrib: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut GetSyncAttribKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut Initialize: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut MakeCurrent: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryAPI: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryContext: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDeviceAttribEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDeviceStringEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDevicesEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDisplayAttribEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDmaBufFormatsEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryDmaBufModifiersEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryStreamKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryStreamu64KHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QueryString: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut QuerySurface: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ReleaseTexImage: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut ReleaseThread: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut StreamAttribKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut SurfaceAttrib: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut SwapBuffers: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut SwapBuffersWithDamageEXT: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut SwapBuffersWithDamageKHR: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut SwapInterval: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut Terminate: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut WaitClient: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut WaitGL: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut WaitNative: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
pub static mut WaitSync: FnPtr = FnPtr {
                f: super::missing_fn_panic as *const raw::c_void,
                is_loaded: false
            };
}

            #[allow(non_snake_case)]
            pub mod BindAPI {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::BindAPI.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::BindAPI = FnPtr::new(metaloadfn(&mut loadfn, "eglBindAPI", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod BindTexImage {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::BindTexImage.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::BindTexImage = FnPtr::new(metaloadfn(&mut loadfn, "eglBindTexImage", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ChooseConfig {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ChooseConfig.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ChooseConfig = FnPtr::new(metaloadfn(&mut loadfn, "eglChooseConfig", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ClientWaitSync {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ClientWaitSync.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ClientWaitSync = FnPtr::new(metaloadfn(&mut loadfn, "eglClientWaitSync", &["eglClientWaitSyncKHR"]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ClientWaitSyncKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ClientWaitSyncKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ClientWaitSyncKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglClientWaitSyncKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CopyBuffers {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CopyBuffers.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CopyBuffers = FnPtr::new(metaloadfn(&mut loadfn, "eglCopyBuffers", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateContext {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateContext.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateContext = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateContext", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateImage {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateImage.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateImage = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateImage", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateImageKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateImageKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateImageKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateImageKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePbufferFromClientBuffer {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePbufferFromClientBuffer.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePbufferFromClientBuffer = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePbufferFromClientBuffer", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePbufferSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePbufferSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePbufferSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePbufferSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePixmapSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePixmapSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePixmapSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePixmapSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePlatformPixmapSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePlatformPixmapSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePlatformPixmapSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePlatformPixmapSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePlatformPixmapSurfaceEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePlatformPixmapSurfaceEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePlatformPixmapSurfaceEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePlatformPixmapSurfaceEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePlatformWindowSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePlatformWindowSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePlatformWindowSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePlatformWindowSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreatePlatformWindowSurfaceEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreatePlatformWindowSurfaceEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreatePlatformWindowSurfaceEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglCreatePlatformWindowSurfaceEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateStreamKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateStreamKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateStreamKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateStreamKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateStreamProducerSurfaceKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateStreamProducerSurfaceKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateStreamProducerSurfaceKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateStreamProducerSurfaceKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateSync {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateSync.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateSync = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateSync", &["eglCreateSync64KHR"]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateSyncKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateSyncKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateSyncKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateSyncKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod CreateWindowSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::CreateWindowSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::CreateWindowSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglCreateWindowSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroyContext {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroyContext.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroyContext = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroyContext", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroyImage {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroyImage.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroyImage = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroyImage", &["eglDestroyImageKHR"]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroyImageKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroyImageKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroyImageKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroyImageKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroyStreamKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroyStreamKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroyStreamKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroyStreamKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroySurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroySurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroySurface = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroySurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroySync {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroySync.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroySync = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroySync", &["eglDestroySyncKHR"]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DestroySyncKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DestroySyncKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DestroySyncKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglDestroySyncKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod DupNativeFenceFDANDROID {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::DupNativeFenceFDANDROID.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::DupNativeFenceFDANDROID = FnPtr::new(metaloadfn(&mut loadfn, "eglDupNativeFenceFDANDROID", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ExportDMABUFImageMESA {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ExportDMABUFImageMESA.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ExportDMABUFImageMESA = FnPtr::new(metaloadfn(&mut loadfn, "eglExportDMABUFImageMESA", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ExportDMABUFImageQueryMESA {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ExportDMABUFImageQueryMESA.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ExportDMABUFImageQueryMESA = FnPtr::new(metaloadfn(&mut loadfn, "eglExportDMABUFImageQueryMESA", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetConfigAttrib {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetConfigAttrib.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetConfigAttrib = FnPtr::new(metaloadfn(&mut loadfn, "eglGetConfigAttrib", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetConfigs {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetConfigs.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetConfigs = FnPtr::new(metaloadfn(&mut loadfn, "eglGetConfigs", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetCurrentContext {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetCurrentContext.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetCurrentContext = FnPtr::new(metaloadfn(&mut loadfn, "eglGetCurrentContext", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetCurrentDisplay {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetCurrentDisplay.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetCurrentDisplay = FnPtr::new(metaloadfn(&mut loadfn, "eglGetCurrentDisplay", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetCurrentSurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetCurrentSurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetCurrentSurface = FnPtr::new(metaloadfn(&mut loadfn, "eglGetCurrentSurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetDisplay {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetDisplay.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetDisplay = FnPtr::new(metaloadfn(&mut loadfn, "eglGetDisplay", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetError {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetError.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetError = FnPtr::new(metaloadfn(&mut loadfn, "eglGetError", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetPlatformDisplay {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetPlatformDisplay.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetPlatformDisplay = FnPtr::new(metaloadfn(&mut loadfn, "eglGetPlatformDisplay", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetPlatformDisplayEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetPlatformDisplayEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetPlatformDisplayEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglGetPlatformDisplayEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetProcAddress {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetProcAddress.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetProcAddress = FnPtr::new(metaloadfn(&mut loadfn, "eglGetProcAddress", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetSyncAttrib {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetSyncAttrib.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetSyncAttrib = FnPtr::new(metaloadfn(&mut loadfn, "eglGetSyncAttrib", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod GetSyncAttribKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::GetSyncAttribKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::GetSyncAttribKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglGetSyncAttribKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod Initialize {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::Initialize.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::Initialize = FnPtr::new(metaloadfn(&mut loadfn, "eglInitialize", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod MakeCurrent {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::MakeCurrent.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::MakeCurrent = FnPtr::new(metaloadfn(&mut loadfn, "eglMakeCurrent", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryAPI {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryAPI.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryAPI = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryAPI", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryContext {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryContext.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryContext = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryContext", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDeviceAttribEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDeviceAttribEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDeviceAttribEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDeviceAttribEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDeviceStringEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDeviceStringEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDeviceStringEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDeviceStringEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDevicesEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDevicesEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDevicesEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDevicesEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDisplayAttribEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDisplayAttribEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDisplayAttribEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDisplayAttribEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDmaBufFormatsEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDmaBufFormatsEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDmaBufFormatsEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDmaBufFormatsEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryDmaBufModifiersEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryDmaBufModifiersEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryDmaBufModifiersEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryDmaBufModifiersEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryStreamKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryStreamKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryStreamKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryStreamKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryStreamu64KHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryStreamu64KHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryStreamu64KHR = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryStreamu64KHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QueryString {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QueryString.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QueryString = FnPtr::new(metaloadfn(&mut loadfn, "eglQueryString", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod QuerySurface {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::QuerySurface.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::QuerySurface = FnPtr::new(metaloadfn(&mut loadfn, "eglQuerySurface", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ReleaseTexImage {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ReleaseTexImage.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ReleaseTexImage = FnPtr::new(metaloadfn(&mut loadfn, "eglReleaseTexImage", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod ReleaseThread {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::ReleaseThread.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::ReleaseThread = FnPtr::new(metaloadfn(&mut loadfn, "eglReleaseThread", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod StreamAttribKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::StreamAttribKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::StreamAttribKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglStreamAttribKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod SurfaceAttrib {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::SurfaceAttrib.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::SurfaceAttrib = FnPtr::new(metaloadfn(&mut loadfn, "eglSurfaceAttrib", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod SwapBuffers {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::SwapBuffers.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::SwapBuffers = FnPtr::new(metaloadfn(&mut loadfn, "eglSwapBuffers", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod SwapBuffersWithDamageEXT {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::SwapBuffersWithDamageEXT.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::SwapBuffersWithDamageEXT = FnPtr::new(metaloadfn(&mut loadfn, "eglSwapBuffersWithDamageEXT", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod SwapBuffersWithDamageKHR {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::SwapBuffersWithDamageKHR.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::SwapBuffersWithDamageKHR = FnPtr::new(metaloadfn(&mut loadfn, "eglSwapBuffersWithDamageKHR", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod SwapInterval {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::SwapInterval.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::SwapInterval = FnPtr::new(metaloadfn(&mut loadfn, "eglSwapInterval", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod Terminate {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::Terminate.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::Terminate = FnPtr::new(metaloadfn(&mut loadfn, "eglTerminate", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod WaitClient {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::WaitClient.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::WaitClient = FnPtr::new(metaloadfn(&mut loadfn, "eglWaitClient", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod WaitGL {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::WaitGL.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::WaitGL = FnPtr::new(metaloadfn(&mut loadfn, "eglWaitGL", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod WaitNative {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::WaitNative.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::WaitNative = FnPtr::new(metaloadfn(&mut loadfn, "eglWaitNative", &[]))
                    }
                }
            }
        

            #[allow(non_snake_case)]
            pub mod WaitSync {
                use super::{storage, metaloadfn};
                use super::__gl_imports::raw;
                use super::FnPtr;

                #[inline]
                #[allow(dead_code)]
                pub fn is_loaded() -> bool {
                    unsafe { storage::WaitSync.is_loaded }
                }

                #[allow(dead_code)]
                pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const raw::c_void {
                    unsafe {
                        storage::WaitSync = FnPtr::new(metaloadfn(&mut loadfn, "eglWaitSync", &[]))
                    }
                }
            }
        
#[inline(never)]
        fn missing_fn_panic() -> ! {
            panic!("egl function was not loaded")
        }
        

        /// Load each OpenGL symbol using a custom load function. This allows for the
        /// use of functions like `glfwGetProcAddress` or `SDL_GL_GetProcAddress`.
        /// ~~~ignore
        /// gl::load_with(|s| glfw.get_proc_address(s));
        /// ~~~
        #[allow(dead_code)]
        pub fn load_with<F>(mut loadfn: F) where F: FnMut(&'static str) -> *const __gl_imports::raw::c_void {
            #[inline(never)]
            fn inner(loadfn: &mut dyn FnMut(&'static str) -> *const __gl_imports::raw::c_void) {
    
BindAPI::load_with(&mut *loadfn);
BindTexImage::load_with(&mut *loadfn);
ChooseConfig::load_with(&mut *loadfn);
ClientWaitSync::load_with(&mut *loadfn);
ClientWaitSyncKHR::load_with(&mut *loadfn);
CopyBuffers::load_with(&mut *loadfn);
CreateContext::load_with(&mut *loadfn);
CreateImage::load_with(&mut *loadfn);
CreateImageKHR::load_with(&mut *loadfn);
CreatePbufferFromClientBuffer::load_with(&mut *loadfn);
CreatePbufferSurface::load_with(&mut *loadfn);
CreatePixmapSurface::load_with(&mut *loadfn);
CreatePlatformPixmapSurface::load_with(&mut *loadfn);
CreatePlatformPixmapSurfaceEXT::load_with(&mut *loadfn);
CreatePlatformWindowSurface::load_with(&mut *loadfn);
CreatePlatformWindowSurfaceEXT::load_with(&mut *loadfn);
CreateStreamKHR::load_with(&mut *loadfn);
CreateStreamProducerSurfaceKHR::load_with(&mut *loadfn);
CreateSync::load_with(&mut *loadfn);
CreateSyncKHR::load_with(&mut *loadfn);
CreateWindowSurface::load_with(&mut *loadfn);
DestroyContext::load_with(&mut *loadfn);
DestroyImage::load_with(&mut *loadfn);
DestroyImageKHR::load_with(&mut *loadfn);
DestroyStreamKHR::load_with(&mut *loadfn);
DestroySurface::load_with(&mut *loadfn);
DestroySync::load_with(&mut *loadfn);
DestroySyncKHR::load_with(&mut *loadfn);
DupNativeFenceFDANDROID::load_with(&mut *loadfn);
ExportDMABUFImageMESA::load_with(&mut *loadfn);
ExportDMABUFImageQueryMESA::load_with(&mut *loadfn);
GetConfigAttrib::load_with(&mut *loadfn);
GetConfigs::load_with(&mut *loadfn);
GetCurrentContext::load_with(&mut *loadfn);
GetCurrentDisplay::load_with(&mut *loadfn);
GetCurrentSurface::load_with(&mut *loadfn);
GetDisplay::load_with(&mut *loadfn);
GetError::load_with(&mut *loadfn);
GetPlatformDisplay::load_with(&mut *loadfn);
GetPlatformDisplayEXT::load_with(&mut *loadfn);
GetProcAddress::load_with(&mut *loadfn);
GetSyncAttrib::load_with(&mut *loadfn);
GetSyncAttribKHR::load_with(&mut *loadfn);
Initialize::load_with(&mut *loadfn);
MakeCurrent::load_with(&mut *loadfn);
QueryAPI::load_with(&mut *loadfn);
QueryContext::load_with(&mut *loadfn);
QueryDeviceAttribEXT::load_with(&mut *loadfn);
QueryDeviceStringEXT::load_with(&mut *loadfn);
QueryDevicesEXT::load_with(&mut *loadfn);
QueryDisplayAttribEXT::load_with(&mut *loadfn);
QueryDmaBufFormatsEXT::load_with(&mut *loadfn);
QueryDmaBufModifiersEXT::load_with(&mut *loadfn);
QueryStreamKHR::load_with(&mut *loadfn);
QueryStreamu64KHR::load_with(&mut *loadfn);
QueryString::load_with(&mut *loadfn);
QuerySurface::load_with(&mut *loadfn);
ReleaseTexImage::load_with(&mut *loadfn);
ReleaseThread::load_with(&mut *loadfn);
StreamAttribKHR::load_with(&mut *loadfn);
SurfaceAttrib::load_with(&mut *loadfn);
SwapBuffers::load_with(&mut *loadfn);
SwapBuffersWithDamageEXT::load_with(&mut *loadfn);
SwapBuffersWithDamageKHR::load_with(&mut *loadfn);
SwapInterval::load_with(&mut *loadfn);
Terminate::load_with(&mut *loadfn);
WaitClient::load_with(&mut *loadfn);
WaitGL::load_with(&mut *loadfn);
WaitNative::load_with(&mut *loadfn);
WaitSync::load_with(&mut *loadfn);

            }

            inner(&mut loadfn)
        }
    
