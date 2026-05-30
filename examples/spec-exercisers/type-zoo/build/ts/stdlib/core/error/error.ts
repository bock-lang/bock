export interface Error {
  message(self): string;
}

export class SimpleError {
  message: string;
  constructor({ message }: { message: string }) {
    this.message = message;
  }
}

interface SimpleError extends Error {}
// impl Error for SimpleError
SimpleError.prototype.message = function(self): string {
  return self.message;
};

export function error(message: string): SimpleError {
  return new SimpleError({ message: message });
}
//# sourceMappingURL=error.ts.map
