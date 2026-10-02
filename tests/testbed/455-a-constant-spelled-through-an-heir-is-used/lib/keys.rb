module Keys
  MAILER_KEY = "mailer:%d"
  STALE_KEY = "stale"
end

module Store
  include Keys
end

class Base
  LIMIT = 10
end

class Child < Base
end
