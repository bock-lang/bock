const Ordering_Less = Object.freeze({ _tag: "Less" });
const Ordering_Equal = Object.freeze({ _tag: "Equal" });
const Ordering_Greater = Object.freeze({ _tag: "Greater" });

// trait Equatable
const Equatable = {
  eq(self, other) {
  },
};

// trait Comparable
const Comparable = {
  compare(self, other) {
  },
};

class Key {
  constructor({ value }) {
    this.value = value;
  }
}

// impl Comparable for Key
Key.prototype.compare = function(self, other) {
  return ((self.value < other.value) ? less : ((self.value === other.value) ? equal : greater));
};

// impl Equatable for Key
Key.prototype.eq = function(self, other) {
  return (self.value === other.value);
};

export function key(value) {
  return new Key({ value: value });
}

export function max(a, b) {
  return (() => {
    switch (a.compare(a, b)._tag) {
      case "Greater": {
        return a;
        break;
      }
      default: {
        return b;
        break;
      }
    }
  })();
}

export function min(a, b) {
  return (() => {
    switch (a.compare(a, b)._tag) {
      case "Less": {
        return a;
        break;
      }
      default: {
        return b;
        break;
      }
    }
  })();
}
//# sourceMappingURL=compare.js.map
