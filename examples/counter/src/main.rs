use iced::widget::{button, column, text, Column};
use iced::{Center, Subscription};

pub fn main() -> iced::Result {
    iced::application(Counter::default, Counter::update, Counter::view)
        .subscription(Counter::subscription)
        .run()
}

#[derive(Default)]
struct Counter {
    value: i64,
}

#[derive(Debug, Clone)]
enum Message {
    Increment,
    Decrement,
    Event(iced::Event),
    WaylandEvent(iced::event::Wayland),
}

impl Counter {
    fn update(&mut self, message: Message) {
        match message {
            Message::Increment => {
                self.value += 1;
            }
            Message::Decrement => {
                self.value -= 1;
            }
            Message::Event(event) => println!("event: {event:?}"),
            Message::WaylandEvent(event) => {
                println!("wayland event: {event:?}")
            }
        }
    }

    fn view(&self) -> Column<'_, Message> {
        column![
            button("Increment").on_press(Message::Increment),
            text(self.value).size(50),
            button("Decrement").on_press(Message::Decrement)
        ]
        .padding(20)
        .align_x(Center)
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            iced::event::listen().map(Message::Event),
            iced::event::listen_wayland().map(Message::WaylandEvent),
        ])
    }
}
