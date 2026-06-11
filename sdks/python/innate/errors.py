"""Innate SDK exceptions."""

from __future__ import annotations


class OutcomeConflictError(Exception):
    """Raised when ``record()`` is called with a conflicting outcome.

    The knowledge layer returns this error when a ``record()`` call tries
    to downgrade an episodic log that is already in a terminal distill_state
    (e.g. 'new' or 'discarded'). See §二·三 record() state machine.
    """

    def __init__(self, message: str = "outcome conflict: record() called on a terminal log state") -> None:
        super().__init__(message)
