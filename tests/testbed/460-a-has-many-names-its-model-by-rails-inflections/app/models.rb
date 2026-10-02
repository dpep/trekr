class Response < ActiveRecord::Base
  def self.answered; end
end

class Person < ActiveRecord::Base
  def self.adults; end
end

class Movie < ActiveRecord::Base
  def self.released; end
end

class Survey < ActiveRecord::Base
  has_many :responses
  has_many :people
  has_many :movies

  def go
    responses.answered
    people.adults
    movies.released
  end
end
