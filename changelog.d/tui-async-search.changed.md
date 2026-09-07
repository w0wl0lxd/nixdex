- Run the TUI search on a blocking worker instead of the event loop. The loop
  kept drawing and reading keys during a database read no longer, so the
  "Searching..." indicator could never be seen and the UI froze for the length
  of the query. A result that arrives for a query the user has already moved
  on from is now dropped rather than resetting the selection.
