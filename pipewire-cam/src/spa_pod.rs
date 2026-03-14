//! SPA POD builder — constructs binary POD structures for PipeWire format negotiation.
//!
//! The SPA POD format is: [u32 size][u32 type][payload...] with 8-byte alignment.

#![allow(dead_code)]

// SPA types
pub const SPA_TYPE_None: u32 = 1;
pub const SPA_TYPE_Bool: u32 = 2;
pub const SPA_TYPE_Id: u32 = 3;
pub const SPA_TYPE_Int: u32 = 4;
pub const SPA_TYPE_Long: u32 = 5;
pub const SPA_TYPE_Float: u32 = 6;
pub const SPA_TYPE_Double: u32 = 7;
pub const SPA_TYPE_String: u32 = 8;
pub const SPA_TYPE_Rectangle: u32 = 12;
pub const SPA_TYPE_Fraction: u32 = 13;
pub const SPA_TYPE_Object: u32 = 15;

// SPA object types
pub const SPA_TYPE_OBJECT_Format: u32 = 0x40002;
pub const SPA_TYPE_OBJECT_ParamBuffers: u32 = 0x40004;
pub const SPA_TYPE_OBJECT_ParamMeta: u32 = 0x40005;

// SPA media types
pub const SPA_MEDIA_TYPE_video: u32 = 2;
pub const SPA_MEDIA_SUBTYPE_raw: u32 = 1;

// SPA video format IDs
pub const SPA_VIDEO_FORMAT_NV12: u32 = 25;

// SPA format keys
pub const SPA_FORMAT_mediaType: u32 = 1;
pub const SPA_FORMAT_mediaSubtype: u32 = 2;
pub const SPA_FORMAT_VIDEO_format: u32 = 0x20001;
pub const SPA_FORMAT_VIDEO_size: u32 = 0x20003;
pub const SPA_FORMAT_VIDEO_framerate: u32 = 0x20004;

// SPA param meta types
pub const SPA_META_Header: u32 = 0;

// SPA param buffer keys
pub const SPA_PARAM_BUFFERS_buffers: u32 = 1;
pub const SPA_PARAM_BUFFERS_blocks: u32 = 2;
pub const SPA_PARAM_BUFFERS_size: u32 = 3;
pub const SPA_PARAM_BUFFERS_stride: u32 = 4;
pub const SPA_PARAM_BUFFERS_dataType: u32 = 9;

pub const SPA_PARAM_META_type: u32 = 1;
pub const SPA_PARAM_META_size: u32 = 2;

// SPA data types (bitmask for dataType)
pub const SPA_DATA_MemPtr: u32 = 1 << 3;

/// Align to 8 bytes
fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Builder for SPA POD binary format.
pub struct PodBuilder {
    buf: Vec<u8>,
}

impl PodBuilder {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(256) }
    }

    /// Get the built pod as bytes. The returned slice points to the pod
    /// WITHOUT the outer size/type header (the object IS the pod).
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume and return the built buffer.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_ne_bytes());
    }

    fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_ne_bytes());
    }

    fn pad_to_8(&mut self) {
        let aligned = align8(self.buf.len());
        self.buf.resize(aligned, 0);
    }

    /// Write a pod header (size, type) and return the offset of the size field
    /// so it can be patched later.
    fn begin_pod(&mut self, type_: u32) -> usize {
        let offset = self.buf.len();
        self.write_u32(0); // placeholder size
        self.write_u32(type_);
        offset
    }

    /// Patch the size field of a pod started with begin_pod.
    fn end_pod(&mut self, offset: usize) {
        let size = (self.buf.len() - offset - 8) as u32;
        self.buf[offset..offset + 4].copy_from_slice(&size.to_ne_bytes());
        self.pad_to_8();
    }

    /// Write an Id pod value.
    fn write_id(&mut self, id: u32) {
        self.write_u32(4); // size
        self.write_u32(SPA_TYPE_Id);
        self.write_u32(id);
        self.pad_to_8();
    }

    /// Write an Int pod value.
    fn write_int(&mut self, v: i32) {
        self.write_u32(4);
        self.write_u32(SPA_TYPE_Int);
        self.write_i32(v);
        self.pad_to_8();
    }

    /// Write a Rectangle pod value (width, height).
    fn write_rectangle(&mut self, width: u32, height: u32) {
        self.write_u32(8);
        self.write_u32(SPA_TYPE_Rectangle);
        self.write_u32(width);
        self.write_u32(height);
    }

    /// Write a Fraction pod value (num, denom).
    fn write_fraction(&mut self, num: u32, denom: u32) {
        self.write_u32(8);
        self.write_u32(SPA_TYPE_Fraction);
        self.write_u32(num);
        self.write_u32(denom);
    }

    /// Write an object property: key + flags + value pod.
    fn begin_property(&mut self, key: u32, flags: u32) {
        self.write_u32(key);
        self.write_u32(flags);
    }

    /// Build an EnumFormat pod for NV12 video at the given size and framerate.
    pub fn build_video_enum_format(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
    ) -> Vec<u8> {
        let mut b = Self::new();

        // Object: Format
        let obj = b.begin_pod(SPA_TYPE_Object);
        b.write_u32(SPA_TYPE_OBJECT_Format); // object type
        b.write_u32(0); // object id (SPA_PARAM_EnumFormat filled by caller)

        // Property: mediaType = video
        b.begin_property(SPA_FORMAT_mediaType, 0);
        b.write_id(SPA_MEDIA_TYPE_video);

        // Property: mediaSubtype = raw
        b.begin_property(SPA_FORMAT_mediaSubtype, 0);
        b.write_id(SPA_MEDIA_SUBTYPE_raw);

        // Property: format = NV12
        b.begin_property(SPA_FORMAT_VIDEO_format, 0);
        b.write_id(SPA_VIDEO_FORMAT_NV12);

        // Property: size = width x height
        b.begin_property(SPA_FORMAT_VIDEO_size, 0);
        b.write_rectangle(width, height);

        // Property: framerate = fps_num/fps_den
        b.begin_property(SPA_FORMAT_VIDEO_framerate, 0);
        b.write_fraction(fps_num, fps_den);

        b.end_pod(obj);
        b.finish()
    }

    /// Build a Buffers param pod.
    pub fn build_buffers(size: u32, min_buffers: i32, max_buffers: i32) -> Vec<u8> {
        let mut b = Self::new();

        let obj = b.begin_pod(SPA_TYPE_Object);
        b.write_u32(SPA_TYPE_OBJECT_ParamBuffers);
        b.write_u32(0);

        // buffers count
        b.begin_property(SPA_PARAM_BUFFERS_buffers, 0);
        b.write_int(min_buffers);

        // blocks
        b.begin_property(SPA_PARAM_BUFFERS_blocks, 0);
        b.write_int(1);

        // size
        b.begin_property(SPA_PARAM_BUFFERS_size, 0);
        b.write_int(size as i32);

        // stride
        b.begin_property(SPA_PARAM_BUFFERS_stride, 0);
        b.write_int(0);

        // dataType = MemPtr
        b.begin_property(SPA_PARAM_BUFFERS_dataType, 0);
        b.write_int(SPA_DATA_MemPtr as i32);

        b.end_pod(obj);
        b.finish()
    }

    /// Build a Meta param pod for header metadata.
    pub fn build_meta_header(header_size: u32) -> Vec<u8> {
        let mut b = Self::new();

        let obj = b.begin_pod(SPA_TYPE_Object);
        b.write_u32(SPA_TYPE_OBJECT_ParamMeta);
        b.write_u32(0);

        b.begin_property(SPA_PARAM_META_type, 0);
        b.write_id(SPA_META_Header);

        b.begin_property(SPA_PARAM_META_size, 0);
        b.write_int(header_size as i32);

        b.end_pod(obj);
        b.finish()
    }
}
