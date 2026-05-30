package main

type Error interface {
	Message(interface{}) string
}

type SimpleError struct {
	Message	string
}

func (s SimpleError) Message(self interface{}) string {
	return self.Message
}

func Error(message string) SimpleError {
	return SimpleError{Message: message}
}
