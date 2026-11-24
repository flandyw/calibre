// Self-contained Swift bridge for Apple's on-device FoundationModels framework
// (macOS 26.0+ with Apple Silicon and Apple Intelligence enabled).
//
// Compiled by build.rs only when the macOS 26+ SDK is present, and weak-linked
// against FoundationModels so binaries still launch cleanly on older macOS versions.

import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

private let CR_APPLE_OK: Int32 = 0
private let CR_APPLE_DEVICE_NOT_ELIGIBLE: Int32 = 1
private let CR_APPLE_NOT_ENABLED: Int32 = 2
private let CR_APPLE_MODEL_NOT_READY: Int32 = 3
private let CR_APPLE_UNSUPPORTED_OS: Int32 = 4
private let CR_APPLE_UNAVAILABLE: Int32 = 5
private let CR_APPLE_CANCELLED: Int32 = -3
private let CR_APPLE_ERROR: Int32 = -1

private func dupString(_ value: String) -> UnsafeMutablePointer<CChar>? {
    value.withCString { strdup($0) }
}

private func writeString(
    _ out: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ value: String
) {
    out?.pointee = dupString(value)
}

@_cdecl("cr_apple_ai_string_free")
public func cr_apple_ai_string_free(_ ptr: UnsafeMutablePointer<CChar>?) {
    if let ptr {
        free(ptr)
    }
}

#if canImport(FoundationModels)
@available(macOS 26.0, *)
private func describeGenerationError(_ error: Error) -> String {
    if let gen = error as? LanguageModelSession.GenerationError {
        switch gen {
        case .exceededContextWindowSize:
            return "Apple Intelligence: this request is too long for the on-device context window; try a shorter excerpt or switch to OpenRouter."
        case .guardrailViolation:
            return "Apple Intelligence declined this request under its built-in safety guidelines."
        case .assetsUnavailable:
            return "Apple Intelligence model assets are unavailable or still downloading."
        case .rateLimited:
            return "Apple Intelligence is temporarily busy; try again in a moment."
        case .unsupportedLanguageOrLocale:
            return "Apple Intelligence does not support this language on-device yet."
        case .refusal:
            return "Apple Intelligence declined to answer this prompt."
        @unknown default:
            return "Apple Intelligence: \(error.localizedDescription)"
        }
    }
    return "Apple Intelligence: \(error.localizedDescription)"
}
#endif

@_cdecl("cr_apple_ai_availability")
public func cr_apple_ai_availability(
    _ reasonOut: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        switch SystemLanguageModel.default.availability {
        case .available:
            return CR_APPLE_OK
        case .unavailable(let reason):
            switch reason {
            case .deviceNotEligible:
                writeString(reasonOut, "This Mac does not support Apple Intelligence (requires Apple Silicon).")
                return CR_APPLE_DEVICE_NOT_ELIGIBLE
            case .appleIntelligenceNotEnabled:
                writeString(reasonOut, "Turn on Apple Intelligence in System Settings to use on-device AI.")
                return CR_APPLE_NOT_ENABLED
            case .modelNotReady:
                writeString(reasonOut, "Apple Intelligence model assets are still downloading.")
                return CR_APPLE_MODEL_NOT_READY
            @unknown default:
                writeString(reasonOut, "Apple Intelligence is currently unavailable on this Mac.")
                return CR_APPLE_UNAVAILABLE
            }
        }
    } else {
        writeString(reasonOut, "Apple Intelligence requires macOS 26 or newer.")
        return CR_APPLE_UNSUPPORTED_OS
    }
    #else
    writeString(reasonOut, "Built without the macOS 26 FoundationModels SDK.")
    return CR_APPLE_UNSUPPORTED_OS
    #endif
}

private final class CompleteBox: @unchecked Sendable {
    var status: Int32 = CR_APPLE_ERROR
    var text: String?
    var error: String?
}

@_cdecl("cr_apple_ai_complete")
public func cr_apple_ai_complete(
    _ system: UnsafePointer<CChar>?,
    _ prompt: UnsafePointer<CChar>?,
    _ outText: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        guard let prompt else {
            writeString(outError, "Apple Intelligence: empty prompt.")
            return CR_APPLE_ERROR
        }
        let systemText = system.map { String(cString: $0) } ?? ""
        let promptText = String(cString: prompt)
        let box = CompleteBox()
        let done = DispatchSemaphore(value: 0)

        Task.detached {
            defer { done.signal() }
            do {
                let session = systemText.isEmpty
                    ? LanguageModelSession()
                    : LanguageModelSession(instructions: systemText)
                let response = try await session.respond(to: promptText)
                box.text = response.content
                box.status = CR_APPLE_OK
            } catch {
                box.error = describeGenerationError(error)
                box.status = CR_APPLE_ERROR
            }
        }

        done.wait()
        if box.status == CR_APPLE_OK, let text = box.text {
            writeString(outText, text)
            return CR_APPLE_OK
        }
        writeString(outError, box.error ?? "Apple Intelligence request failed.")
        return box.status
    } else {
        writeString(outError, "Apple Intelligence requires macOS 26 or newer.")
        return CR_APPLE_UNSUPPORTED_OS
    }
    #else
    writeString(outError, "Built without the macOS 26 FoundationModels SDK.")
    return CR_APPLE_UNSUPPORTED_OS
    #endif
}

public typealias CrAppleDeltaCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<CChar>?
) -> Int32

private final class StreamBox: @unchecked Sendable {
    var status: Int32 = CR_APPLE_OK
    var error: String?
    let callback: CrAppleDeltaCallback?
    let ctx: UnsafeMutableRawPointer?

    init(callback: CrAppleDeltaCallback?, ctx: UnsafeMutableRawPointer?) {
        self.callback = callback
        self.ctx = ctx
    }
}

@_cdecl("cr_apple_ai_stream")
public func cr_apple_ai_stream(
    _ system: UnsafePointer<CChar>?,
    _ prompt: UnsafePointer<CChar>?,
    _ onDelta: CrAppleDeltaCallback?,
    _ ctx: UnsafeMutableRawPointer?,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        guard let prompt else {
            writeString(outError, "Apple Intelligence: empty prompt.")
            return CR_APPLE_ERROR
        }
        let systemText = system.map { String(cString: $0) } ?? ""
        let promptText = String(cString: prompt)
        let box = StreamBox(callback: onDelta, ctx: ctx)
        let done = DispatchSemaphore(value: 0)

        Task.detached {
            defer { done.signal() }
            do {
                let session = systemText.isEmpty
                    ? LanguageModelSession()
                    : LanguageModelSession(instructions: systemText)
                let stream = session.streamResponse(to: promptText)
                var previous = ""
                for try await snapshot in stream {
                    let current = snapshot.content
                    if current.count > previous.count, current.hasPrefix(previous) {
                        let delta = String(current.dropFirst(previous.count))
                        previous = current
                        if let cb = box.callback {
                            let rc = delta.withCString { cb(box.ctx, $0) }
                            if rc != 0 {
                                box.status = CR_APPLE_CANCELLED
                                return
                            }
                        }
                    } else if current != previous {
                        previous = current
                    }
                }
                box.status = CR_APPLE_OK
            } catch {
                box.error = describeGenerationError(error)
                box.status = CR_APPLE_ERROR
            }
        }

        done.wait()
        if box.status == CR_APPLE_OK {
            return CR_APPLE_OK
        }
        if let err = box.error {
            writeString(outError, err)
        }
        return box.status
    } else {
        writeString(outError, "Apple Intelligence requires macOS 26 or newer.")
        return CR_APPLE_UNSUPPORTED_OS
    }
    #else
    writeString(outError, "Built without the macOS 26 FoundationModels SDK.")
    return CR_APPLE_UNSUPPORTED_OS
    #endif
}
