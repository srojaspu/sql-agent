use super::state::AppState;

impl AppState {
    /// Version-bumping input helpers so the render loop can dirty-check
    /// keystrokes without polling the string every frame.
    pub fn push_input(&mut self, c: char) {
        if self.cursor_index >= self.input.len() {
            self.input.push(c);
            self.cursor_index = self.input.len();
        } else {
            self.input.insert(self.cursor_index, c);
            self.cursor_index += c.len_utf8();
        }
        self.bump();
    }

    pub fn pop_input(&mut self) {
        if self.cursor_index > 0 && !self.input.is_empty() {
            let prev = self.input[..self.cursor_index]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor_index = prev;
            self.bump();
        }
    }

    pub fn delete_input(&mut self) {
        if self.cursor_index < self.input.len() {
            self.input.remove(self.cursor_index);
            self.bump();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_index > 0 {
            self.cursor_index = self.input[..self.cursor_index]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.bump();
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_index < self.input.len() {
            let next = self.input[self.cursor_index..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_index + i)
                .unwrap_or_else(|| self.input.len());
            self.cursor_index = next;
            self.bump();
        }
    }

    pub fn move_cursor_home(&mut self) {
        if self.cursor_index != 0 {
            self.cursor_index = 0;
            self.bump();
        }
    }

    pub fn move_cursor_end(&mut self) {
        if self.cursor_index != self.input.len() {
            self.cursor_index = self.input.len();
            self.bump();
        }
    }

    pub fn clear_input(&mut self) {
        if !self.input.is_empty() || self.cursor_index != 0 {
            self.input.clear();
            self.cursor_index = 0;
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Session;

    fn make_state() -> AppState {
        AppState::new(Session::new())
    }

    #[test]
    fn app_input_cursor_navigation_and_editing() {
        let mut app = make_state();
        assert_eq!(app.cursor_index, 0);
        app.push_input('a');
        app.push_input('c');
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor_index, 2);

        // Move left and insert 'b'
        app.move_cursor_left();
        assert_eq!(app.cursor_index, 1);
        app.push_input('b');
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor_index, 2);

        // Move home
        app.move_cursor_home();
        assert_eq!(app.cursor_index, 0);

        // Delete at 0 deletes 'a'
        app.delete_input();
        assert_eq!(app.input, "bc");
        assert_eq!(app.cursor_index, 0);

        // Move end
        app.move_cursor_end();
        assert_eq!(app.cursor_index, 2);

        // Backspace deletes 'c'
        app.pop_input();
        assert_eq!(app.input, "b");
        assert_eq!(app.cursor_index, 1);

        // Clear input
        app.clear_input();
        assert_eq!(app.input, "");
        assert_eq!(app.cursor_index, 0);
    }
}
