import Foundation
import GrouseCore

/// The two server→client form families, both answered by request id (CONTRACT §5).
enum ServerFormKind: String {
    case recipe
    case elicitation
}

/// One choice in a form field. goose uses two shapes: a flat `enum` array and titled
/// `oneOf[].const`; the title is for the human, the value goes back on the wire.
struct ServerFormOption: Identifiable {
    let value: String
    let title: String
    var id: String { value }
}

/// One row of a form. The server authors the labels and the choices; this client only
/// renders them, and keeps the raw value as a string until it builds the answer — that
/// keeps the form UI independent of goose's JSON schema dialect.
struct ServerFormField: Identifiable {
    let name: String
    let title: String
    let detail: String
    let type: String
    let required: Bool
    let options: [ServerFormOption]
    let defaultValue: String

    var id: String { name }
}

/// A form prompt as sheet state. `id` is the CORE's request id, not a local counter: two
/// wires can park a form at the same time, and the answer must name the one being filled.
struct ServerFormPrompt: Identifiable {
    let requestId: String
    let kind: ServerFormKind
    let owner: String
    let title: String
    let fields: [ServerFormField]

    var id: String { requestId }

    // MARK: - Decoding

    static func recipe(owner: String, requestId: String, payload: String) -> ServerFormPrompt? {
        guard let array = json(payload) as? [Any] else { return nil }
        let fields = array.compactMap { entry -> ServerFormField? in
            guard let object = entry as? [String: Any],
                  let key = string(object["key"]), !key.isEmpty else { return nil }
            let description = string(object["description"]) ?? ""
            let fallback = string(object["default"])
            return ServerFormField(
                name: key,
                title: key,
                detail: fallback.map { "\(description) (default: \($0))" } ?? description,
                type: string(object["input_type"]) ?? "string",
                required: string(object["requirement"]) == "required",
                options: plainOptions(object["options"]),
                defaultValue: fallback ?? ""
            )
        }
        return ServerFormPrompt(requestId: requestId, kind: .recipe, owner: owner,
                                title: "This recipe needs parameters", fields: fields)
    }

    static func elicitation(owner: String, requestId: String, payload: String) -> ServerFormPrompt? {
        guard let object = json(payload) as? [String: Any] else { return nil }
        let properties = object["properties"] as? [String: Any] ?? [:]
        let required = Set(plainStrings(object["required"]))
        let fields = properties.keys.sorted().compactMap { name -> ServerFormField? in
            guard let raw = properties[name] as? [String: Any] else { return nil }
            return ServerFormField(
                name: name,
                title: string(raw["title"]) ?? name,
                detail: string(raw["description"]) ?? "",
                type: string(raw["type"]) ?? "string",
                required: required.contains(name),
                options: elicitationOptions(raw),
                defaultValue: string(raw["default"]) ?? ""
            )
        }
        return ServerFormPrompt(requestId: requestId, kind: .elicitation, owner: owner,
                                title: string(object["title"]) ?? "Input requested",
                                fields: fields)
    }

    // MARK: - Answers

    /// The recipe shim wants `{action: "submit"|"cancel", values: {key: STRING}}`;
    /// an ACP elicitation wants an object of typed JSON values. Build both from the
    /// same text fields so the sheet does not have to know which wire parked the form.
    func answerJSON(action: String, textValues: [String: String]) -> String {
        var values: [String: Any] = [:]
        for field in fields {
            let raw = textValues[field.name] ?? field.defaultValue
            if kind == .recipe {
                values[field.name] = raw
            } else {
                values[field.name] = typed(raw, type: field.type)
            }
        }
        guard let data = try? JSONSerialization.data(withJSONObject: values,
                                                     options: [.sortedKeys]),
              let json = String(data: data, encoding: .utf8) else { return "{}" }
        return json
    }

    private func typed(_ raw: String, type: String) -> Any {
        switch type {
        case "boolean": return Bool(raw) ?? false
        case "integer": return Int(raw) ?? 0
        case "number": return Double(raw) ?? 0
        default: return raw
        }
    }

    // MARK: - JSON helpers

    private static func json(_ text: String) -> Any? {
        guard let data = text.data(using: .utf8) else { return nil }
        return try? JSONSerialization.jsonObject(with: data)
    }

    private static func string(_ value: Any?) -> String? {
        if let text = value as? String { return text }
        if let number = value as? NSNumber { return number.stringValue }
        return nil
    }

    private static func plainStrings(_ value: Any?) -> [String] {
        guard let array = value as? [Any] else { return [] }
        return array.compactMap { string($0) }
    }

    private static func plainOptions(_ value: Any?) -> [ServerFormOption] {
        plainStrings(value).map { ServerFormOption(value: $0, title: $0) }
    }

    private static func elicitationOptions(_ object: [String: Any]) -> [ServerFormOption] {
        let oneOf = (object["oneOf"] as? [Any] ?? []).compactMap { entry -> ServerFormOption? in
            guard let option = entry as? [String: Any],
                  let value = string(option["const"]), !value.isEmpty else { return nil }
            return ServerFormOption(value: value, title: string(option["title"]) ?? value)
        }
        if !oneOf.isEmpty { return oneOf }
        return plainOptions(object["enum"])
    }
}
