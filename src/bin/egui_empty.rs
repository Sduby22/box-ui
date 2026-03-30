fn main() -> eframe::Result<()> {
    eframe::run_ui_native(
        "egui empty window",
        eframe::NativeOptions::default(),
        |_ctx, _frame| {},
    )
}
