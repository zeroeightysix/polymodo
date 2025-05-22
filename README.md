# polymodo

A **super-high performance**, **daemonized**, **fuzzy** 
* application launcher,
* ~~anything-finder~~ (_wip_),
* generic window in the middle of your screen to do things!

Primarily made to alleviate pains in other application launchers, and therefore with a focus on:
* uncompromising user-interface: the year is 2025, and we can do better than a CPU-drawn paged list of application names and, if lucky, icons.
  * polymodo uses [**egui**](https://github.com/emilk/egui/), a rust-native UI toolkit
  * polymodo shows more than just the application name: users deserve the description, category, and **additional actions** an application offers!
* startup and matching speed:
  * no one wants to wait a second each time their launcher opens, nor are slow search results acceptable: polymodo optimizes both metrics to deliver a **truly fast** launching experience.
* application configuration made for users:
  * (_wip_) tweak polymodo's settings not from hidden configuration files written in an arcane language, but from the UI itself!

### a generic window?

Although its main purpose is to be an application launcher, polymodo is designed to run any number of "apps", in parallel: it serves mostly as a common process for caching results and handling UI/windowing.

