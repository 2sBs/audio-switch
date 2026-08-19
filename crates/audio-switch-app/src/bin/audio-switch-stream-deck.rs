#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    audio_switch::stream_deck::run(&std::env::args().collect::<Vec<_>>())
}
