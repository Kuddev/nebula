import Foundation
import Vision

guard CommandLine.arguments.count == 3 else {
    fputs("usage: macos-text IMAGE EXPECTED_TEXT\n", stderr)
    exit(2)
}

do {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.recognitionLanguages = ["en-US"]
    let handler = VNImageRequestHandler(url: URL(fileURLWithPath: CommandLine.arguments[1]))
    try handler.perform([request])
    let expected = CommandLine.arguments[2].uppercased()
    let found = (request.results ?? []).contains { observation in
        observation.topCandidates(1).contains { candidate in
            candidate.string.uppercased().contains(expected)
        }
    }
    exit(found ? 0 : 1)
} catch {
    fputs("screenshot text recognition failed: \(error)\n", stderr)
    exit(2)
}
