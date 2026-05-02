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
            Some(0) => {
                self.selected = None;
                self.scroll_offset = 0;
            }
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
            None if total_items > 0 => {
                self.selected = Some(0);
            }
            None => {}
            Some(n) if n + 1 < total_items => {
                self.selected = Some(n + 1);
                if n + 1 >= self.scroll_offset + max_visible {
                    self.scroll_offset = n + 1 - max_visible + 1;
                }
            }
            _ => {}
        }
    }

    pub fn move_page_up(&mut self, max_visible: usize) {
        match self.selected {
            Some(0) => {
                self.selected = None;
                self.scroll_offset = 0;
            }
            Some(n) => {
                let new = n.saturating_sub(max_visible);
                self.selected = Some(new);
                self.scroll_offset = self.scroll_offset.min(new);
            }
            None => {}
        }
    }

    pub fn move_page_down(&mut self, total_items: usize, max_visible: usize) {
        match self.selected {
            None if total_items > 0 => {
                self.selected = Some(0);
            }
            None => {}
            Some(n) => {
                let Some(last) = total_items.checked_sub(1) else {
                    return;
                };
                let new = n.saturating_add(max_visible).min(last);
                if new == n {
                    return;
                }

                self.selected = Some(new);
                if new >= self.scroll_offset + max_visible {
                    self.scroll_offset = new + 1 - max_visible;
                }
                self.scroll_offset = self
                    .scroll_offset
                    .min(total_items.saturating_sub(max_visible));
            }
        }
    }

    pub fn move_home(&mut self, total_items: usize) {
        if total_items == 0 {
            return;
        }

        self.selected = Some(0);
        self.scroll_offset = 0;
    }

    pub fn move_end(&mut self, total_items: usize, max_visible: usize) {
        let Some(last) = total_items.checked_sub(1) else {
            return;
        };

        self.selected = Some(last);
        self.scroll_offset = total_items.saturating_sub(max_visible);
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
    fn test_move_up_at_zero_resets_scroll_offset() {
        let mut state = OverlayState::new();
        state.selected = Some(0);
        state.scroll_offset = 5; // leftover from prior scrolling
        state.move_up();
        assert_eq!(state.selected, None);
        assert_eq!(
            state.scroll_offset, 0,
            "scroll_offset must reset when deselecting"
        );
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
    fn test_page_up_from_none_is_noop() {
        let mut state = OverlayState::new();
        state.move_page_up(DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_up_below_max_visible_clamps_to_zero() {
        let mut state = OverlayState::new();
        state.selected = Some(3);
        state.scroll_offset = 1;
        state.move_page_up(DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_up_at_zero_deselects() {
        let mut state = OverlayState::new();
        state.selected = Some(0);
        state.scroll_offset = 5;
        state.move_page_up(DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_up_full_page_step() {
        let mut state = OverlayState::new();
        state.selected = Some(15);
        state.scroll_offset = 6;
        state.move_page_up(DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn test_page_up_keeps_invariant() {
        let mut state = OverlayState::new();
        state.selected = Some(25);
        state.scroll_offset = 16;
        state.move_page_up(DEFAULT_MAX_VISIBLE);
        let selected = state.selected.unwrap();
        assert!(state.scroll_offset <= selected);
        assert!(selected < state.scroll_offset + DEFAULT_MAX_VISIBLE);
    }

    #[test]
    fn test_page_down_from_none_selects_zero() {
        let mut state = OverlayState::new();
        state.move_page_down(50, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_down_empty_list_is_noop() {
        let mut state = OverlayState::new();
        state.move_page_down(0, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_down_clamps_at_last() {
        let mut state = OverlayState::new();
        state.selected = Some(48);
        state.scroll_offset = 40;
        state.move_page_down(50, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(49));
        assert_eq!(state.scroll_offset, 40);
    }

    #[test]
    fn test_page_down_at_last_is_noop() {
        let mut state = OverlayState::new();
        state.selected = Some(49);
        state.scroll_offset = 40;
        state.move_page_down(50, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(49));
        assert_eq!(state.scroll_offset, 40);
    }

    #[test]
    fn test_page_down_full_step_scrolls_viewport() {
        let mut state = OverlayState::new();
        state.selected = Some(5);
        state.scroll_offset = 0;
        state.move_page_down(100, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(15));
        assert_eq!(state.scroll_offset, 6);
    }

    #[test]
    fn test_page_down_short_list_no_viewport_change() {
        let mut state = OverlayState::new();
        state.selected = Some(2);
        state.move_page_down(5, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(4));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_page_down_keeps_invariant() {
        let mut state = OverlayState::new();
        state.selected = Some(25);
        state.scroll_offset = 16;
        state.move_page_down(100, DEFAULT_MAX_VISIBLE);
        let selected = state.selected.unwrap();
        assert!(state.scroll_offset <= selected);
        assert!(selected < state.scroll_offset + DEFAULT_MAX_VISIBLE);
    }

    #[test]
    fn test_home_empty_list_is_noop() {
        let mut state = OverlayState::new();
        state.move_home(0);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_home_from_none_selects_zero() {
        let mut state = OverlayState::new();
        state.move_home(50);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_home_from_middle_resets_scroll() {
        let mut state = OverlayState::new();
        state.selected = Some(20);
        state.scroll_offset = 11;
        state.move_home(50);
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_end_empty_list_is_noop() {
        let mut state = OverlayState::new();
        state.move_end(0, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, None);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_end_from_none_selects_last() {
        let mut state = OverlayState::new();
        state.move_end(50, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(49));
        assert_eq!(state.scroll_offset, 40);
    }

    #[test]
    fn test_end_short_list_no_scroll() {
        let mut state = OverlayState::new();
        state.move_end(5, DEFAULT_MAX_VISIBLE);
        assert_eq!(state.selected, Some(4));
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_end_keeps_invariant() {
        let mut state = OverlayState::new();
        state.move_end(50, DEFAULT_MAX_VISIBLE);
        let selected = state.selected.unwrap();
        assert!(state.scroll_offset <= selected);
        assert!(selected < state.scroll_offset + DEFAULT_MAX_VISIBLE);
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
