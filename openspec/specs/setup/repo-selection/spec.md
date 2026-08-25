# setup/repo-selection Specification

## Purpose
Covers how a person sees which repositories on this machine may upload chats,
which ones cannot and why, and how they change that list without retyping it.

## Requirements

### Requirement: Status names repositories that cannot upload

`status` SHALL list repositories that hold a destination file but cannot produce
a route, each with the reason, alongside the routes that work. A machine where
every destination is usable SHALL say so rather than printing an empty heading.

#### Scenario: A destination file is missing a required value

- **WHEN** `status` runs on a machine where one repository's destination file
  has no API key
- **THEN** that repository is listed as blocked with the missing value named,
  and the working routes are still listed

#### Scenario: A repository is allowed but has no destination

- **WHEN** an entry on the allow list matches no repository with a destination
  file
- **THEN** `status` says so for that entry

### Requirement: The allow list can be changed without being retyped

`configure` SHALL accept adding and removing named repositories, keeping every
other entry untouched. The existing replace-everything form SHALL keep its
current meaning. Adding a repository already on the list, or removing one that
is absent, SHALL succeed without changing anything else.

#### Scenario: A repository is added to an existing list

- **WHEN** `configure --add-repo <folder>` runs against a list of five entries
- **THEN** the list holds six entries and the original five are unchanged

#### Scenario: A repository is removed

- **WHEN** `configure --remove-repo <folder>` runs
- **THEN** only that entry is gone

#### Scenario: Adding and removing in one command

- **WHEN** both flags are given in one invocation
- **THEN** every named addition and removal is applied to the stored list

#### Scenario: The replace form still replaces

- **WHEN** `configure --allow-repo <folder>` runs
- **THEN** the stored list holds exactly the named repositories
