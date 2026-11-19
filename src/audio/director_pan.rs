//! Stereo position (pan) of the [`Director`].
//!
//! This file is an `impl Director` extension, like `director_audio.rs`.
//!
//! # Where a pan comes from
//!
//! A library sound has its own pan ([`CatalogSound::pan`], `-1.0` left ..= `1.0`
//! right, `0.0` centre). A scene item may override it: a non-zero
//! `[[library.scenes.items]] pan` replaces the sound's pan while that scene is
//! active, `0.0` (the default) means "use the sound's own pan". [`resolve_pan`]
//! is that rule in one place.
//!
//! # When it is applied
//!
//! The engine keeps one pan per sound and the mixer slews the gains, so a
//! change in the middle of playback does not click.
//!
//! * Before a sound starts (scene start, delayed start, manual play, stinger):
//!   the pan is set first, so the track begins at its position.
//! * On scene activation, for the sounds of the scene that already play (a
//!   retuned sound moves to the new scene's pan).
//! * In real time: [`Director::set_sound_pan`] for one sound, and
//!   [`Director::apply_pans`] after the library or a scene item was edited.
//!   Emitter instances are skipped there: their pan is the emitter's own jitter.

use crate::audio::director::{CatalogSound, Director, DirectorError, Playback, PlaybackResult};
use crate::audio::engine::SoundId;

/// Clamp a pan into `-1.0..=1.0`; NaN and infinities fold to the centre.
pub fn sanitize_pan(pan: f32) -> f32 {
    if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// The pan a sound plays at: the scene item's pan when it is non-zero (an
/// override), otherwise the sound's own. Both are clamped.
pub fn resolve_pan(sound_pan: f32, item_pan: Option<f32>) -> f32 {
    match item_pan.map(sanitize_pan) {
        Some(item) if item != 0.0 => item,
        _ => sanitize_pan(sound_pan),
    }
}

impl Director {
    /// The pan `sound` has in `scene` (`None` = no scene): the item's override
    /// or the sound's own pan. `0.0` for a sound the catalog does not know.
    pub fn pan_in_scene(&self, scene: Option<&str>, sound: SoundId) -> f32 {
        let Some(entry) = self.catalog_sound(sound) else {
            return 0.0;
        };
        let item_pan = scene.and_then(|id| self.scene(id)).and_then(|scene| {
            scene
                .items
                .iter()
                .find(|item| item.sound == entry.id)
                .map(|item| item.pan)
        });
        resolve_pan(entry.pan, item_pan)
    }

    /// The pan `sound` has right now: [`pan_in_scene`](Self::pan_in_scene) of
    /// the active scene.
    pub fn effective_pan(&self, sound: SoundId) -> f32 {
        self.pan_in_scene(self.active_scene(), sound)
    }

    /// Set the pan a sound starts at; called right before `play`. A sound the
    /// catalog does not know is left alone.
    pub(crate) fn pan_before_start<P: Playback + ?Sized>(
        &self,
        pb: &P,
        scene: Option<&str>,
        sound: SoundId,
    ) -> PlaybackResult {
        if self.catalog_sound(sound).is_none() {
            return Ok(());
        }
        pb.set_pan(sound, self.pan_in_scene(scene, sound))
    }

    /// Move the sounds of `scene` that already play to the scene's pans.
    pub(crate) fn apply_scene_pans<P: Playback + ?Sized>(&self, pb: &P, scene: &str) {
        let Some(scene) = self.scene(scene) else {
            return;
        };
        for item in &scene.items {
            let Some(entry) = self.catalog.iter().find(|e| e.id == item.sound) else {
                continue;
            };
            if pb.is_playing(entry.sound) {
                self.push_pan(pb, entry, resolve_pan(entry.pan, Some(item.pan)));
            }
        }
    }

    /// Push the effective pan of every sound that plays now (after the library
    /// or a scene item changed). Emitter instances keep their random pan.
    pub fn apply_pans<P: Playback + ?Sized>(&self, pb: &P) {
        let scene = self.active_scene();
        for entry in &self.catalog {
            if !pb.is_playing(entry.sound) {
                continue;
            }
            if scene.is_some_and(|id| self.emitter_instance_of(id, entry.sound)) {
                continue;
            }
            self.push_pan(pb, entry, self.pan_in_scene(scene, entry.sound));
        }
    }

    /// Set a sound's own pan (`-1.0..=1.0`, clamped), remembered in the catalog.
    /// A scene item with its own pan keeps winning while its scene is active.
    pub fn set_sound_pan<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
        pan: f32,
    ) -> Result<(), DirectorError> {
        if pan.is_nan() {
            return Err(DirectorError::Invalid("pan is not a number".into()));
        }
        let pan = sanitize_pan(pan);
        if let Some(entry) = self.catalog.iter_mut().find(|e| e.sound == sound) {
            entry.pan = pan;
        }
        pb.set_pan(sound, self.effective_pan(sound))?;
        Ok(())
    }

    fn push_pan<P: Playback + ?Sized>(&self, pb: &P, entry: &CatalogSound, pan: f32) {
        if let Err(e) = pb.set_pan(entry.sound, pan) {
            tracing::warn!("Could not pan {}: {e}", entry.id);
        }
    }
}
