class Field
  def run
    nested_schema
  end

  def nested_schema
  end
end

class ArrayField < Field
  def nested_schema
  end
end

class PlainField < Field
end

module Stamping
  def nested_schema
  end
end

class StampedField < Field
  include Stamping
end

class Solo
  def run
    helper
  end

  def helper
  end
end
