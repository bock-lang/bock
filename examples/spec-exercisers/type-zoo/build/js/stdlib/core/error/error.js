// trait Error
const Error = {
  message(self) {
  },
};

class SimpleError {
  constructor({ message }) {
    this.message = message;
  }
}

// impl Error for SimpleError
SimpleError.prototype.message = function(self) {
  return self.message;
};

export function error(message) {
  return new SimpleError({ message: message });
}
//# sourceMappingURL=error.js.map
