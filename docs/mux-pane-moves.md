# Mux pane moves

Moving the last pane out of a server tab can leave a ghost tab in an attached GUI. Closing that
ghost tab can kill the real pane that was moved.

`wezterm-attention` does not install a topology keybinding or attempt an automatic workaround.
Attention identity follows the server pane through its published full address, but it does not own
WezTerm tab or pane topology.

Before closing a ghost tab, confirm the real pane is visible in its destination tab or another
attached GUI. If the topology is unclear, detach the affected GUI and reattach instead of closing
the ghost tab.
