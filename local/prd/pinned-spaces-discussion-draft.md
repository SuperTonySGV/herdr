# Upstream Discussion draft — pinned spaces

**Status: NOT POSTED.** Anthony asked for this to be drafted only. To post it
yourself: GitHub → herdrdev/herdr → Discussions → New discussion → Ideas.

Per `CONTRIBUTING.md`, feature requests belong in Discussions, not Issues, and
should describe the problem rather than the implementation. This draft
deliberately contains no patch, no pseudocode, and no root-cause analysis — the
fork's implementation is kept out of it on purpose.

Suggested category: **Ideas**

---

**Title:** Let a space stay in the list when its last tab closes

**Body:**

I keep the same handful of repos open in Herdr all day and use the space list as
my main way of moving between them. The thing that keeps breaking that habit is
that a space only exists as long as it has a tab. When I close the last tab in a
space — or the agent running in it exits on its own — the whole space disappears
from the list, and getting back to that repo means creating a new space and
navigating to the directory again.

The result is that my most-used repos are the ones most likely to vanish, because
they're the ones I actually finish work in. Spaces I abandon half-done stick
around; spaces I close cleanly don't.

What I'd like is a way to mark a space as one I always want in the list, so that
losing its last tab re-seeds it with a fresh tab in the same directory instead of
closing it. Unmarked spaces would keep today's behavior exactly.

I'm not asking for a launcher or a fuzzy-picker — those exist and they're a
different interaction. The space list is already the thing I look at; I'd just
like the important rows to stay put.

Two things I'd want to get right if this were built:

- A space should never be left with no tabs even briefly, since so much of the
  app assumes a space has an active tab.
- The mark should persist across restarts along with the rest of the session, and
  should be anchored to the repo the space represents rather than to a specific
  tab or pane.

Happy to hear if this cuts against how you think about spaces — I realize the
current behavior may well be deliberate.
