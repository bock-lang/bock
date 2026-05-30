export interface From<T> {
  from(value: T): this;
}

export interface Into<T> {
  into(self): T;
}

export interface TryFrom<T> {
  try_from(value: T): Result<this, ConvertError>;
}

export interface Displayable {
  to_string(self): string;
}

export class ConvertError {
  message: string;
  constructor({ message }: { message: string }) {
    this.message = message;
  }
}

export function convertError(message: string): ConvertError {
  return new ConvertError({ message: message });
}

export class Celsius {
  degrees: number;
  constructor({ degrees }: { degrees: number }) {
    this.degrees = degrees;
  }
}

export class Fahrenheit {
  degrees: number;
  constructor({ degrees }: { degrees: number }) {
    this.degrees = degrees;
  }
}

interface Fahrenheit extends From {}
// impl From for Fahrenheit
Fahrenheit.prototype.from = function(value: Celsius): Fahrenheit {
  return new Fahrenheit({ degrees: ((value.degrees * 1.8) + 32.0) });
};
//# sourceMappingURL=convert.ts.map
