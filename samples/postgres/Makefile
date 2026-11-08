.NOTPARALLEL:
.PHONY: setup preflight fetch runtime-build process-test-native process-test-guest process-test check pg-kernel pg-native pg-guest pg-package sql-test ram-demo storage-build storage-package storage-init storage-config storage-test persistent-demo recovery-test benchmark-build benchmark validate

setup:
	$(MAKE) preflight fetch pg-native pg-guest storage-build storage-package storage-init check

preflight fetch:
	python3 runtime.py $@

runtime-build:
	python3 runtime.py build

process-test-native:
	python3 runtime.py test-native

process-test-guest:
	python3 runtime.py test-guest

check:
	python3 -m unittest discover -s tests -p 'test_*.py'

process-test:
	$(MAKE) preflight fetch runtime-build check process-test-native process-test-guest

pg-kernel:
	python3 postgres.py kernel

pg-native:
	python3 postgres.py build-native

pg-guest:
	python3 postgres.py build-guest

pg-package:
	python3 postgres.py package

sql-test:
	python3 postgres.py test

ram-demo:
	$(MAKE) preflight fetch
	$(MAKE) pg-kernel pg-native pg-guest pg-package check sql-test

storage-build:
	python3 storage.py build

storage-package:
	python3 storage.py package

storage-init:
	python3 storage.py init

storage-config:
	python3 storage.py configure

storage-test:
	python3 storage.py test

persistent-demo:
	$(MAKE) preflight fetch storage-build storage-package storage-config check storage-test

recovery-test:
	python3 recovery.py test

benchmark-build:
	python3 evaluate.py build

benchmark:
	python3 evaluate.py test

validate:
	$(MAKE) process-test ram-demo persistent-demo recovery-test benchmark-build benchmark
