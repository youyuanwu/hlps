.NOTPARALLEL:
.PHONY: preflight fetch build test-native test-guest check phase1 phase2 pg-kernel pg-native pg-guest pg-package pg-test phase3 storage-build storage-package storage-init storage-config storage-test

preflight fetch build test-native test-guest:
	python3 phase1.py $@

check:
	python3 -m unittest discover -s tests -p 'test_*.py'

phase1:
	$(MAKE) preflight
	$(MAKE) fetch
	$(MAKE) build
	$(MAKE) check test-native test-guest

pg-kernel:
	python3 phase2.py kernel

pg-native:
	python3 phase2.py build-native

pg-guest:
	python3 phase2.py build-guest

pg-package:
	python3 phase2.py package

pg-test:
	python3 phase2.py test

phase2:
	$(MAKE) preflight fetch
	$(MAKE) pg-kernel pg-native pg-guest pg-package check pg-test

storage-build:
	python3 phase3.py build

storage-package:
	python3 phase3.py package

storage-init:
	python3 phase3.py init

storage-config:
	python3 phase3.py configure

storage-test:
	python3 phase3.py test

phase3:
	$(MAKE) preflight fetch storage-build storage-package storage-config check storage-test
