// trait From
const From = {
  from(value) {
  },
};

// trait Into
const Into = {
  into(self) {
  },
};

// trait TryFrom
const TryFrom = {
  try_from(value) {
  },
};

// trait Displayable
const Displayable = {
  to_string(self) {
  },
};

class ConvertError {
  constructor({ message }) {
    this.message = message;
  }
}

export function convertError(message) {
  return new ConvertError({ message: message });
}

class Celsius {
  constructor({ degrees }) {
    this.degrees = degrees;
  }
}

class Fahrenheit {
  constructor({ degrees }) {
    this.degrees = degrees;
  }
}

// impl From for Fahrenheit
Fahrenheit.prototype.from = function(value) {
  return new Fahrenheit({ degrees: ((value.degrees * 1.8) + 32.0) });
};
//# sourceMappingURL=convert.js.map
