# Pipeline

This is modelled as a state machine, mapping `Input -> Translator -> Item`, along with an `Invalid` state, which is reachable if either

- input can't be parsed as a translator, or
- translator cannot fetch the item after trying for `MAX_RETRIES` time.
