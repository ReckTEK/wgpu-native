use crate::{conv, native, WGPUBufferImpl};
use std::{ffi::c_void, ptr};

pub(crate) struct MappedRange {
    start: u64,
    end: u64,
    writable: bool,
}

#[derive(Default)]
pub(crate) struct MappedRanges {
    generation: u64,
    ranges: Vec<MappedRange>,
}

impl MappedRange {
    fn conflicts(&self, other: &Self) -> bool {
        (self.writable || other.writable)
            && self.start < self.end
            && other.start < other.end
            && self.start < other.end
            && other.start < self.end
    }
}

fn access<R>(
    buffer: &WGPUBufferImpl,
    offset: usize,
    size: usize,
    writable: bool,
    retain: bool,
    operation: impl FnOnce(*mut u8, usize) -> R,
) -> Option<R> {
    let size = if size == conv::WGPU_WHOLE_MAP_SIZE {
        if !retain {
            log::error!("Mapped copies do not accept WGPU_WHOLE_MAP_SIZE");
            return None;
        }
        None
    } else {
        Some(size as u64)
    };
    let mut borrowed = buffer.mapped_ranges.lock();
    let result = buffer.inner.clone().with_mapped_range(
        offset as u64,
        size,
        writable,
        |data, count, generation| {
            if borrowed.generation != generation {
                borrowed.ranges.clear();
                borrowed.generation = generation;
            }
            let ranges = &mut borrowed.ranges;
            let range = MappedRange {
                start: offset as u64,
                end: offset as u64 + count,
                writable,
            };
            if ranges.iter().any(|prior| range.conflicts(prior)) {
                return None;
            }

            let result = operation(data.as_ptr(), count as usize);
            if retain && count != 0 {
                // Repeated const ranges may overlap; retaining only their union bounds
                // would incorrectly reject a later writable range inside a gap.
                if !ranges.iter().any(|prior| {
                    prior.writable == writable
                        && prior.start <= range.start
                        && prior.end >= range.end
                }) {
                    ranges.push(range);
                }
            }
            Some(result)
        },
    );

    drop(borrowed);
    match result {
        Ok(Some(result)) => Some(result),
        Ok(None) => {
            log::error!("Mapped range overlaps an outstanding writable range");
            None
        }
        Err(error) => {
            log::error!("Invalid mapped range: {error}");
            None
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn wgpuBufferGetMapState(
    buffer: native::WGPUBuffer,
) -> native::WGPUBufferMapState {
    let _buffer_guard = crate::retain_handle(buffer);
    let buffer = buffer.as_ref().expect("invalid buffer");
    match buffer.inner.clone().map_status() {
        Ok(wgc::resource::BufferMapStatus::Pending) => native::WGPUBufferMapState_Pending,
        Ok(wgc::resource::BufferMapStatus::Mapped { .. }) => native::WGPUBufferMapState_Mapped,
        _ => native::WGPUBufferMapState_Unmapped,
    }
}

#[no_mangle]
pub unsafe extern "C" fn wgpuBufferGetConstMappedRange(
    buffer: native::WGPUBuffer,
    offset: usize,
    size: usize,
) -> *const c_void {
    let _buffer_guard = crate::retain_handle(buffer);
    access(
        buffer.as_ref().expect("invalid buffer"),
        offset,
        size,
        false,
        true,
        |data, _| data.cast_const().cast(),
    )
    .unwrap_or(ptr::null())
}

#[no_mangle]
pub unsafe extern "C" fn wgpuBufferGetMappedRange(
    buffer: native::WGPUBuffer,
    offset: usize,
    size: usize,
) -> *mut c_void {
    let _buffer_guard = crate::retain_handle(buffer);
    access(
        buffer.as_ref().expect("invalid buffer"),
        offset,
        size,
        true,
        true,
        |data, _| data.cast(),
    )
    .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn wgpuBufferReadMappedRange(
    buffer: native::WGPUBuffer,
    offset: usize,
    data: *mut c_void,
    size: usize,
) -> native::WGPUStatus {
    let _buffer_guard = crate::retain_handle(buffer);
    if data.is_null() && size != 0 {
        log::error!("Mapped copy destination is null");
        return native::WGPUStatus_Error;
    }
    match access(
        buffer.as_ref().expect("invalid buffer"),
        offset,
        size,
        false,
        false,
        |source, count| {
            if count != 0 {
                ptr::copy(source, data.cast(), count);
            }
        },
    ) {
        Some(()) => native::WGPUStatus_Success,
        None => native::WGPUStatus_Error,
    }
}

#[no_mangle]
pub unsafe extern "C" fn wgpuBufferWriteMappedRange(
    buffer: native::WGPUBuffer,
    offset: usize,
    data: *const c_void,
    size: usize,
) -> native::WGPUStatus {
    let _buffer_guard = crate::retain_handle(buffer);
    if data.is_null() && size != 0 {
        log::error!("Mapped copy source is null");
        return native::WGPUStatus_Error;
    }
    match access(
        buffer.as_ref().expect("invalid buffer"),
        offset,
        size,
        true,
        false,
        |destination, count| {
            if count != 0 {
                ptr::copy(data.cast(), destination, count);
            }
        },
    ) {
        Some(()) => native::WGPUStatus_Success,
        None => native::WGPUStatus_Error,
    }
}

#[cfg(test)]
mod tests {
    use super::MappedRange;

    #[test]
    fn overlap_requires_a_writable_nonempty_intersection() {
        let read = MappedRange {
            start: 8,
            end: 24,
            writable: false,
        };
        let write = MappedRange {
            start: 16,
            end: 32,
            writable: true,
        };
        let adjacent = MappedRange {
            start: 24,
            end: 32,
            writable: true,
        };
        let empty = MappedRange {
            start: 16,
            end: 16,
            writable: true,
        };
        assert!(!read.conflicts(&read));
        assert!(read.conflicts(&write));
        assert!(!read.conflicts(&adjacent));
        assert!(!read.conflicts(&empty));
    }
}
