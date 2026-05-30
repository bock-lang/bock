export type Ordering = Ordering_Less | Ordering_Equal | Ordering_Greater;

interface Ordering_Less { readonly _tag: "Less"; }
const Ordering_Less: Ordering_Less = Object.freeze({ _tag: "Less" as const });
interface Ordering_Equal { readonly _tag: "Equal"; }
const Ordering_Equal: Ordering_Equal = Object.freeze({ _tag: "Equal" as const });
interface Ordering_Greater { readonly _tag: "Greater"; }
const Ordering_Greater: Ordering_Greater = Object.freeze({ _tag: "Greater" as const });

export interface Equatable {
  eq(self, other: this): boolean;
}

export interface Comparable {
  compare(self, other: this): Ordering;
}

export class Key {
  value: number;
  constructor({ value }: { value: number }) {
    this.value = value;
  }
}

interface Key extends Comparable {}
// impl Comparable for Key
Key.prototype.compare = function(self, other: Key): Ordering {
  return ((self.value < other.value) ? less : ((self.value === other.value) ? equal : greater));
};

interface Key extends Equatable {}
// impl Equatable for Key
Key.prototype.eq = function(self, other: Key): boolean {
  return (self.value === other.value);
};

export function key(value: number): Key {
  return new Key({ value: value });
}

export function max<T extends Comparable>(a: T, b: T): T {
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

export function min<T extends Comparable>(a: T, b: T): T {
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
//# sourceMappingURL=compare.ts.map
