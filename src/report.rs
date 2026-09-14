use std::any::Any;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::native;

#[derive(Clone, Copy)]
pub enum Kind {
    Surfaces,
    Adapters,
    Devices,
    Queues,
    PipelineLayouts,
    ShaderModules,
    BindGroupLayouts,
    BindGroups,
    CommandBuffers,
    RenderBundles,
    RenderPipelines,
    ComputePipelines,
    PipelineCaches,
    QuerySets,
    Buffers,
    Textures,
    TextureViews,
    Samplers,
}

type Slot = Weak<dyn Any + Send + Sync>;

pub struct Tracker {
    registries: Mutex<[Vec<Slot>; 18]>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            registries: Mutex::new(std::array::from_fn(|_| Vec::new())),
        }
    }
}

impl Tracker {
    pub fn track<T: Any + Send + Sync>(&self, kind: Kind, handle: &Arc<T>) {
        let erased: Arc<dyn Any + Send + Sync> = handle.clone();
        let weak = Arc::downgrade(&erased);
        let mut registries = self.registries.lock();
        let slots = &mut registries[kind as usize];
        if slots.iter().any(|slot| slot.ptr_eq(&weak)) {
            return;
        }
        if let Some(slot) = slots.iter_mut().find(|slot| slot.strong_count() == 0) {
            *slot = weak;
        } else {
            slots.push(weak);
        }
    }

    pub fn untrack<T>(&self, kind: Kind, handle: &T) {
        let address = std::ptr::from_ref(handle).cast::<()>();
        let mut registries = self.registries.lock();
        if let Some(slot) = registries[kind as usize]
            .iter_mut()
            .find(|slot| slot.as_ptr().cast::<()>() == address)
        {
            *slot = Weak::<()>::new();
        }
    }

    pub fn snapshot(&self) -> native::WGPUGlobalReport {
        let registries = self.registries.lock();
        let report = |kind: Kind| {
            let slots = &registries[kind as usize];
            let occupied = slots.iter().filter(|slot| slot.strong_count() != 0).count();
            native::WGPURegistryReport {
                numAllocated: occupied,
                numKeptFromUser: occupied,
                numReleasedFromUser: slots.len() - occupied,
                elementSize: std::mem::size_of::<Slot>(),
            }
        };
        native::WGPUGlobalReport {
            surfaces: report(Kind::Surfaces),
            hub: native::WGPUHubReport {
                adapters: report(Kind::Adapters),
                devices: report(Kind::Devices),
                queues: report(Kind::Queues),
                pipelineLayouts: report(Kind::PipelineLayouts),
                shaderModules: report(Kind::ShaderModules),
                bindGroupLayouts: report(Kind::BindGroupLayouts),
                bindGroups: report(Kind::BindGroups),
                commandBuffers: report(Kind::CommandBuffers),
                renderBundles: report(Kind::RenderBundles),
                renderPipelines: report(Kind::RenderPipelines),
                computePipelines: report(Kind::ComputePipelines),
                pipelineCaches: report(Kind::PipelineCaches),
                querySets: report(Kind::QuerySets),
                buffers: report(Kind::Buffers),
                textures: report(Kind::Textures),
                textureViews: report(Kind::TextureViews),
                samplers: report(Kind::Samplers),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Tracker};
    use std::sync::Arc;

    #[test]
    fn reports_live_handles_and_reuses_released_slots() {
        let tracker = Tracker::default();
        let first = Arc::new(1_u32);
        let second = Arc::new(2_u32);
        tracker.track(Kind::Buffers, &first);
        tracker.track(Kind::Buffers, &first);
        tracker.track(Kind::Buffers, &second);
        let report = tracker.snapshot().hub.buffers;
        assert_eq!(
            (
                report.numAllocated,
                report.numKeptFromUser,
                report.numReleasedFromUser
            ),
            (2, 2, 0)
        );

        let retained = first.clone();
        drop(first);
        assert_eq!(tracker.snapshot().hub.buffers.numAllocated, 2);
        drop(retained);
        let report = tracker.snapshot().hub.buffers;
        assert_eq!(
            (
                report.numAllocated,
                report.numKeptFromUser,
                report.numReleasedFromUser
            ),
            (1, 1, 1)
        );

        let third = Arc::new(3_u32);
        tracker.track(Kind::Buffers, &third);
        let report = tracker.snapshot().hub.buffers;
        assert_eq!(
            (
                report.numAllocated,
                report.numKeptFromUser,
                report.numReleasedFromUser
            ),
            (2, 2, 0)
        );
        assert_eq!(tracker.snapshot().hub.textures.numAllocated, 0);
        drop(second);
        drop(third);
        assert_eq!(tracker.snapshot().hub.buffers.numReleasedFromUser, 2);
    }

    #[test]
    fn tracking_does_not_keep_handles_alive_and_instances_are_independent() {
        let tracker = Tracker::default();
        let other = Tracker::default();
        let handle = Arc::new(5_u32);
        tracker.track(Kind::Devices, &handle);
        assert_eq!(Arc::strong_count(&handle), 1);
        assert_eq!(other.snapshot().hub.devices.numAllocated, 0);
        drop(handle);
        assert_eq!(tracker.snapshot().hub.devices.numAllocated, 0);
    }

    #[test]
    fn consumed_handles_release_their_slot_before_the_wrapper_is_dropped() {
        let tracker = Tracker::default();
        let handle = Arc::new(5_u32);
        tracker.track(Kind::CommandBuffers, &handle);
        tracker.untrack(Kind::CommandBuffers, handle.as_ref());
        let report = tracker.snapshot().hub.commandBuffers;
        assert_eq!(report.numAllocated, 0);
        assert_eq!(report.numReleasedFromUser, 1);
        assert_eq!(Arc::strong_count(&handle), 1);
        tracker.untrack(Kind::CommandBuffers, handle.as_ref());
        assert_eq!(tracker.snapshot().hub.commandBuffers.numReleasedFromUser, 1);
    }

    #[test]
    fn concurrent_registration_and_release_preserve_occupancy() {
        let tracker = Tracker::default();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for index in 0..1000 {
                        let handle = Arc::new(index);
                        tracker.track(Kind::Textures, &handle);
                        let report = tracker.snapshot().hub.textures;
                        assert!(report.numAllocated >= 1);
                        assert_eq!(report.numAllocated, report.numKeptFromUser);
                    }
                });
            }
        });
        let report = tracker.snapshot().hub.textures;
        assert_eq!(report.numAllocated, 0);
        assert!((1..=4).contains(&report.numReleasedFromUser));
    }
}
