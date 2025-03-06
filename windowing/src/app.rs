pub trait App {
    fn render(&mut self, ctx: &egui::Context);
}

