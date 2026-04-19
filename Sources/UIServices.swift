import Foundation

final class UIServices {
    let candidatePanel: CandidatePanel
    let inputSourceMonitor: InputSourceMonitor

    init(candidatePanel: CandidatePanel = CandidatePanel(),
         inputSourceMonitor: InputSourceMonitor = InputSourceMonitor()) {
        self.candidatePanel = candidatePanel
        self.inputSourceMonitor = inputSourceMonitor
    }

    func startMonitoring() {
        inputSourceMonitor.startMonitoring()
    }
}
