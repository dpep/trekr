module Expectations
  infect :assert_empty, :must_be_empty
  infect :assert_mocked, :must_verify if
    defined?(infect)
end

class Object
  include Expectations
end

class Console
  BINDING_IMPL = [<<-METHOD, __FILE__, __LINE__ + 1].freeze
    # A binding with no locals; the definition is eval'd where it lands.
    def __console__
      binding
    end
  METHOD
end

class Object
  def __binding__
    return class_eval("binding", __FILE__, __LINE__) if is_a?(Module)

    self.class.class_eval(*Console::BINDING_IMPL)
  end
end

class Gate
  def open
    :open
  end
end
