pub trait App {
    fn update(&mut self, ctx: &egui::Context);
}