use super::types::{CandidateAction, KeyResponse, MarkedText, Submode};
use super::InputSession;

impl InputSession<'_> {
    pub(super) fn toggle_submode(&mut self) -> KeyResponse {
        let current_submode = self.submode();
        let new_submode = match current_submode {
            Submode::Japanese => Submode::English,
            Submode::English => Submode::Japanese,
        };

        if self.is_composing() {
            // Flush pending romaji before switching
            if !self.comp().pending.is_empty() {
                self.flush();
            }

            // Undo boundary space if nothing was typed since the last toggle
            self.comp().prefix.undo_boundary_space();

            // Crystallize the current segment into prefix.
            match current_submode {
                Submode::Japanese => {
                    let c = self.comp();
                    let frozen = if c.candidates.selected < c.candidates.surfaces.len() {
                        let reading = c.kana.clone();
                        let surface = c.candidates.surfaces[c.candidates.selected].clone();
                        self.record_history(reading, surface.clone());
                        surface
                    } else {
                        self.comp().kana.clone()
                    };
                    self.comp().prefix.push_str(&frozen);
                }
                Submode::English => {
                    let kana = self.comp().kana.clone();
                    self.comp().prefix.push_str(&kana);
                }
            }
            // Clear the current segment for the new submode
            let c = self.comp();
            c.kana.clear();
            c.pending.clear();
            c.candidates.clear();
            c.stability.reset();

            // Programmer mode: insert space at submode boundary
            c.prefix.has_boundary_space = false;
            if self.programmer_mode && !self.comp().prefix.is_empty() {
                if let Some(last) = self.comp().prefix.text.chars().last() {
                    let last_is_ascii = last.is_ascii();
                    let should_insert = (current_submode == Submode::Japanese
                        && new_submode == Submode::English
                        && !last_is_ascii)
                        || (current_submode == Submode::English
                            && new_submode == Submode::Japanese
                            && last_is_ascii
                            && last != ' ');
                    if should_insert {
                        self.comp().prefix.text.push(' ');
                        self.comp().prefix.has_boundary_space = true;
                    }
                }
            }

            self.comp().submode = new_submode;

            let display = self.comp().display();
            let mut resp = KeyResponse::consumed();
            if !display.is_empty() {
                resp.marked = Some(MarkedText {
                    text: display,
                    dashed: new_submode == Submode::English,
                });
            }
            resp.candidates = CandidateAction::Hide;
            if !self.history_records.is_empty() {
                resp.side_effects.save_history = true;
            }
            resp
        } else {
            // Idle: just toggle the idle_submode
            self.idle_submode = new_submode;
            KeyResponse::consumed()
        }
    }
}
