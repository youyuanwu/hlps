.NOTPARALLEL:
.PHONY: preflight fetch build test-native test-guest check phase1

preflight fetch build test-native test-guest:
	python3 phase1.py $@

check:
	python3 -m unittest discover -s tests -p 'test_*.py'

phase1:
	$(MAKE) preflight
	$(MAKE) fetch
	$(MAKE) build
	$(MAKE) check test-native test-guest
