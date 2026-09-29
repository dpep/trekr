class Person < ActiveRecord::Base
  def self.purge
    delete_by(name: "x")
  end
end

class Engine
  def delete_by(*args); end
end

class Garage
  delegate :delete_by, to: :engine

  def engine
    Engine.new
  end
end

Person.delete_by(id: 1)
Garage.new.delete_by(id: 1)
