class Shape
  def area
    raise NotImplementedError
  end
end

class Square < Shape
  def area
    4
  end
end

class Circle < Shape
  def area
    3
  end
end

class Vehicle
  def wheels
    4
  end
end

class Bike < Vehicle
  def wheels
    2
  end
end

class Car < Vehicle
end

class Report
  def run
    Square.new.area + Circle.new.area + Bike.new.wheels + Car.new.wheels
  end
end
