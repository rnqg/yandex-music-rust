from pathlib import Path

application = Path(defines["application"])
name = application.name

format = "UDZO"
files = [str(application)]
symlinks = {"Applications": "/Applications"}
hide_extensions = [name]
background = "builtin-arrow"
window_rect = ((140, 160), (640, 240))
icon_size = 112
text_size = 12
icon_locations = {name: (142, 116), "Applications": (502, 116)}
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
