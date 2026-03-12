pub const DEFAULT_MAX_VISIBLE: usize = 10;
pub const DEFAULT_MIN_POPUP_WIDTH: u16 = 20;
pub const DEFAULT_MAX_POPUP_WIDTH: u16 = 60;

#[derive(Debug, Clone)]
pub struct OverlayState {
    pub selected: Option<usize>,
    pub scroll_offset: usize,
}

impl OverlayState {
    pub fn new() -> Self {
        Self {
            selected: None,
            scroll_offset: 0,
        }
    }

    pub fn move_up(&mut self) {
        match self.selected {
            Some(0) => self.selected = None,
            Some(n) => {
                self.selected = Some(n - 1);
                if n - 1 < self.scroll_offset {
                    self.scroll_offset = n - 1;
                }
            }
            None => {}
        }
    }

    pub fn move_down(&mut self, total_items: usize, max_visible: usize) {
        match self.selected {
            None => {
                if total_items > 0 {
                    self.selected = Some(0);
                }
            }
            Some(n) if n + 1 < total_items => {
                self.selected = Some(n + 1);
                if n + 1 >= self.scroll_offset + max_visible {
                    self.scroll_offset = n + 1 - max_visible + 1;
                }
            }
            _ => {}
        }
    }

    pub fn reset(&mut self) {
        self.selected = None;
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
    pub scroll_deficit: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_down_from_none_selects_first() {
        let mut state = OverlayState::new();
        assert_eq!(state.selected, None);
        state.move_down(5, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn test_move_down_increments() {
        let mut state = OverlayState::new();
        state.selected = Some(0);
        state.move_down(5, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn test_move_up_decrements() {
        let mut state = OverlayState::new();
        state.selected = Some(1);
        state.move_up();
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn test_move_up_at_zero_deselects() {
        let mut state = OverlayState::new();
        state.selected = Some(0);
        state.move_up();
        assert_eq!(state.selected, None);
    }

    #[test]
    fn test_move_up_at_none_stays_none() {
        let mut state = OverlayState::new();
        state.move_up();
        assert_eq!(state.selected, None);
    }

    #[test]
    fn test_move_down_at_end_stays() {
        let mut state = OverlayState::new();
        state.selected = Some(4);
        state.move_down(5, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(4));
    }

    #[test]
    fn test_scroll_offset_on_move_down() {
        let mut state = OverlayState::new();
        // First move_down goes None -> Some(0), then 0->1, 1->2, ...
        for _ in 0..DEFAULT_MAX_VISIBLE + 3 {
            state.move_down(20, DEFAULT_MAX_VISIBLE);
        }
        // None + (MAX_VISIBLE + 3) moves = Some(MAX_VISIBLE + 2)
        assert_eq!(state.selected, Some(DEFAULT_MAX_VISIBLE + 2));
        assert!(state.scroll_offset > 0);
        assert!(state.selected.unwrap() < state.scroll_offset + DEFAULT_MAX_VISIBLE);
    }

    #[test]
    fn test_scroll_offset_on_move_up() {
        let mut state = OverlayState::new();
        state.selected = Some(5);
        state.scroll_offset = 5;
        state.move_up();
        assert_eq!(state.selected, Some(4));
        assert_eq!(state.scroll_offset, 4);
    }

    #[test]
    fn test_reset() {
        let mut state = OverlayState::new();
        state.selected = Some(7);
        state.scroll_offset = 3;
        state.reset();
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_custom_max_visible() {
        let mut state = OverlayState::new();
        let custom_max = 3;
        // 6 moves: None->0, 0->1, 1->2, 2->3, 3->4, 4->5
        for _ in 0..6 {
            state.move_down(20, custom_max);
        }
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.scroll_offset, 3);
        assert!(state.selected.unwrap() < state.scroll_offset + custom_max);
    }
}
