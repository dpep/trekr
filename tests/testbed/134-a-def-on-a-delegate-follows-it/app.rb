class Person < ActiveRecord::Base
end

class Engine
  def delete_by(*args); end
end

class Garage
  delegate :delete_by, to: :engine

  def engine
    @engine
  end
end

Person.delete_by(id: 1)
Person.insert_all([])
Garage.new.delete_by(id: 1)
