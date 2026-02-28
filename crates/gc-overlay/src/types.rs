pub const MAX_VISIBLE: usize = 10;
pub const MIN_POPUP_WIDTH: u16 = 20;
pub const MAX_POPUP_WIDTH: u16 = 60;

#[derive(Debug, Clone)]
pub struct OverlayState {
    pub selected: usize,
    pub scroll_offset: usize,
}

impl OverlayState {
    pub fn new() -> Self {
        Self {
            selected: 0,
            scroll_offset: 0,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    pub fn move_down(&mut self, total_items: usize) {
        if self.selected + 1 < total_items {
            self.selected += 1;
            if self.selected >= self.scroll_offset + MAX_VISIBLE {
                self.scroll_offset = self.selected - MAX_VISIBLE + 1;
            }
        }
    }

    pub fn reset(&mut self) {
        self.selected = 0;
        self.scroll_offset = 0;
    }
}

impl Default for OverlayState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct PopupLayout {
    pub start_row: u16,
    pub start_col: u16,
    pub width: u16,
    pub height: u16,
    pub renders_above: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_down_increments() {
        let mut state = OverlayState::new();
        state.move_down(5);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn test_move_up_decrements() {
        let mut state = OverlayState::new();
        state.selected = 1;
        state.move_up();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_move_up_at_zero_stays() {
        let mut state = OverlayState::new();
        state.move_up();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_move_down_at_end_stays() {
        let mut state = OverlayState::new();
        state.selected = 4;
        state.move_down(5);
        assert_eq!(state.selected, 4);
    }

    #[test]
    fn test_scroll_offset_on_move_down() {
        let mut state = OverlayState::new();
        // Move down past MAX_VISIBLE
        for _ in 0..MAX_VISIBLE + 2 {
            state.move_down(20);
        }
        assert_eq!(state.selected, MAX_VISIBLE + 2);
        assert!(state.scroll_offset > 0);
        assert!(state.selected < state.scroll_offset + MAX_VISIBLE);
    }

    #[test]
    fn test_scroll_offset_on_move_up() {
        let mut state = OverlayState::new();
        state.selected = 5;
        state.scroll_offset = 5;
        state.move_up();
        assert_eq!(state.selected, 4);
        assert_eq!(state.scroll_offset, 4);
    }

    #[test]
    fn test_reset() {
        let mut state = OverlayState::new();
        state.selected = 7;
        state.scroll_offset = 3;
        state.reset();
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll_offset, 0);
    }
}
