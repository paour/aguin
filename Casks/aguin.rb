cask "aguin" do
  version "0.1.2"
  sha256 "aed4549d25d319699bb9b6f001234fce8ef1af94e92536a08453e954ee41ad3b"

  url "https://github.com/paour/aguin/releases/download/v#{version}/Aguin-#{version}-macos-universal.zip"
  name "Aguin"
  desc "Viewer for the atuin shell history and coding-agent threads"
  homepage "https://github.com/paour/aguin"

  depends_on :macos

  app "Aguin.app"
  binary "#{appdir}/Aguin.app/Contents/MacOS/aguin"

  # The app has only an ad-hoc signature, so Gatekeeper blocks it while it is quarantined.
  postflight_steps do
    run "/usr/bin/xattr",
        args:           ["-dr", "com.apple.quarantine", "{{appdir}}/Aguin.app"],
        writable_paths: ["{{appdir}}/Aguin.app"]
  end

  zap trash: "~/Library/Application Support/aguin"
end
