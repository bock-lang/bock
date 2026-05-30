from dataclasses import dataclass

# trait Error
class Error:
    def message(self, self) -> str:
        pass

@dataclass
class SimpleError(Error):
    message: str

    def message(self, self) -> str:
        return self.message

def error(message: str) -> SimpleError:
    return SimpleError(message=message)
