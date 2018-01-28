#[derive(Clone, Debug, PartialEq)]
pub struct WindowStack<T> {
    stack: Vec<T>,
}


impl<T: Copy> WindowStack<T> {
    pub fn new(default: T) -> Self {
        WindowStack {
            stack: vec![default],
        }
    }

    pub fn push(&mut self, window: T) {
        self.stack.push(window);
    }

    pub fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    pub fn top(&self) -> T {
        *self.stack.last().unwrap()
    }

    pub fn windows(&self) -> impl Iterator<Item=&T> {
        self.stack.iter()
    }
}
