import SwiftUI

/// Chooses an audio device by ID. A saved device that is currently missing
/// stays selected and is marked unavailable, so a disconnect never silently
/// changes the route.
struct DevicePicker: View {
    let title: String
    @Binding var selection: String?
    let devices: [Endpoint]

    var body: some View {
        Picker(title, selection: $selection) {
            Text("Not selected").tag(String?.none)
            if let selection, !devices.contains(where: { $0.id == selection }) {
                Label("Unavailable device", systemImage: "exclamationmark.triangle")
                    .tag(String?.some(selection))
            }
            Divider()
            ForEach(devices, id: \.id) { device in
                Text(device.name).tag(String?.some(device.id))
            }
        }
    }
}

extension CoreModel {
    /// A binding that saves each change to one setting immediately.
    func binding<Value>(_ keyPath: WritableKeyPath<AppConfig, Value>) -> Binding<Value> {
        Binding(
            get: { self.config[keyPath: keyPath] },
            set: { value in self.update { $0[keyPath: keyPath] = value } }
        )
    }
}
