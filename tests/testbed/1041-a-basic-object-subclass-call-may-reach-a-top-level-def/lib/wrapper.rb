module Tryish
  def attempt
    :module
  end
end

class Object
  include Tryish
end

class Wrapper < BasicObject
  include ::Tryish

  def go
    attempt
  end
end
