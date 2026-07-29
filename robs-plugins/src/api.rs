pub fn plugin_api_version() -> u32 {
    1
}

#[repr(C)]
pub struct SourceDefinition {
    pub id: *const i8,
    pub type_: *const i8,
    pub display_name: *const i8,
    pub flags: u32,
}

pub const SOURCE_FLAG_VIDEO: u32 = 1 << 0;
pub const SOURCE_FLAG_AUDIO: u32 = 1 << 1;
pub const SOURCE_FLAG_ASYNC: u32 = 1 << 2;
pub const SOURCE_FLAG_INTERACTION: u32 = 1 << 3;