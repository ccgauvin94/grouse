import SwiftUI

/// The server's form, rendered with native controls. There is no client-side schema here:
/// the field labels, order, defaults and choices all come from the core's payload, and the
/// answers go back with the REQUEST ID that arrived with it.
struct ServerFormSheet: View {
    let prompt: ServerFormPrompt
    let onAnswer: (String, [String: String]) -> Void

    @State private var values: [String: String] = [:]

    private var missingRequired: Bool {
        prompt.fields.contains { field in
            field.required && (values[field.name] ?? field.defaultValue).isEmpty
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(prompt.title).font(.headline)
            if !prompt.owner.isEmpty {
                Text("Wire: \(prompt.owner)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(prompt.fields) { field in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 3) {
                                Text(field.title)
                                    .font(.callout.weight(.medium))
                                if field.required {
                                    Text("*").foregroundStyle(.red)
                                }
                            }
                            if !field.detail.isEmpty {
                                Text(field.detail)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .textSelection(.enabled)
                            }
                            if field.options.isEmpty {
                                TextField(field.type, text: Binding(
                                    get: { values[field.name] ?? field.defaultValue },
                                    set: { values[field.name] = $0 }
                                ))
                                .textFieldStyle(.roundedBorder)
                            } else {
                                Picker("", selection: Binding(
                                    get: { values[field.name] ?? field.defaultValue },
                                    set: { values[field.name] = $0 }
                                )) {
                                    ForEach(field.options) { option in
                                        Text(option.title).tag(option.value)
                                    }
                                }
                                .labelsHidden()
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 280)

            Divider()

            HStack {
                Spacer()
                if prompt.kind == .elicitation {
                    Button("Decline") { onAnswer("decline", values) }
                }
                Button("Cancel") { onAnswer("cancel", values) }
                Button(prompt.kind == .recipe ? "Submit" : "Accept") {
                    onAnswer(prompt.kind == .recipe ? "submit" : "accept", values)
                }
                .keyboardShortcut(.defaultAction)
                .disabled(missingRequired)
            }
        }
        .padding(18)
        .frame(width: 440)
        .interactiveDismissDisabled()
        .onAppear {
            // One-way push from the prompt into local state: QQC2/SwiftUI text fields
            // break bindings on edit, and every request is a fresh sheet identity anyway.
            var initial: [String: String] = [:]
            for field in prompt.fields where !field.defaultValue.isEmpty {
                initial[field.name] = field.defaultValue
            }
            values = initial
        }
    }
}
