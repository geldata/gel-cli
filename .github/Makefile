.PHONY: all

ROOT = $(dir $(realpath $(firstword $(MAKEFILE_LIST))))

all: workflows/nightly.yml workflows/release.yml workflows/testing.yml workflows/dryrun.yml

workflows/%.yml: workflows.src/%.tpl.yml workflows.src/%.targets.yml workflows.src/build.inc.yml
	$(ROOT)/workflows.src/render.py $* $*.targets.yml
