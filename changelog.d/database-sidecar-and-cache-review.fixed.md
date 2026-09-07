Regenerate sidecars when an update removes frames. `frame_hashes_diff` compared only the frames present in the new database, so shrinking it reported no changes and regeneration was skipped: the sidecars kept entries for packages that no longer existed. A frame that disappeared now counts as a change.

The ngram candidate cache evicts by recency rather than insertion order. `NgramCache::get` never touched the recency list, so a query hit thousands of times was still evicted once 256 distinct patterns had been inserted after it — the daemon's repeated-query workload is exactly the case the cache exists for.

`read_frame_hashes` rejects a declared frame count above `MAX_FRAME_COUNT` before allocating, matching the cap `parse_seek_table` already applies.

`Reader::prefault` and `prefault_mmap` describe what they actually do. Both claimed to touch every page and return with the mapping resident, which is true only without the `huge_pages` feature; with it they issue an asynchronous `MADV_WILLNEED` hint and return before the pages arrive.
