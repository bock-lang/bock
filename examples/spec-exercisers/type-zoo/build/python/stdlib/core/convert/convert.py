from dataclasses import dataclass

# trait From
class From:
    def from(self, value: T) -> Self:
        pass

# trait Into
class Into:
    def into(self, self) -> T:
        pass

# trait TryFrom
class TryFrom:
    def try_from(self, value: T) -> Result[Self, ConvertError]:
        pass

# trait Displayable
class Displayable:
    def to_string(self, self) -> str:
        pass

@dataclass
class ConvertError:
    message: str

def convert_error(message: str) -> ConvertError:
    return ConvertError(message=message)

@dataclass
class Celsius:
    degrees: float

@dataclass
class Fahrenheit(From):
    degrees: float

    def from(self, value: Celsius) -> Fahrenheit:
        return Fahrenheit(degrees=((value.degrees * 1.8) + 32.0))
