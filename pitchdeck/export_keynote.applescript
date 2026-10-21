on run argv
  set inputPath to item 1 of argv
  set outputPath to item 2 of argv
  tell application "Keynote"
    activate
    open POSIX file inputPath
    repeat 30 times
      if (count of documents) > 0 then exit repeat
      delay 1
    end repeat
    if (count of documents) is 0 then error "Keynote did not open the presentation"
    set deckDocument to front document
    export deckDocument to POSIX file outputPath as PDF
    close deckDocument saving no
  end tell
end run
