use iced::widget::text;
use iced::Element;

fn main() -> iced::Result {
    iced::application(|| (), update, view)
        .title("iced empty window")
        .run()
}

fn update(_state: &mut (), _message: ()) {}

fn view(_state: &()) -> Element<'_, ()> {
    text("").into()
}
