module Widget
  # Signs requests with the key in @private_key.
  class Signer
    def sign(text)
    end
  end

  # Parses directives, e.g. `as: "@private"`.
  class Directives
    def parse
    end
  end

  # A client.
  #
  # @example
  #   # @api private
  #   client.call
  class Client
    def call
    end
  end

  # Fetches a page.
  #
  # Internal: callers should prefer #fetch! when they want errors raised.
  def self.fetch
  end

  # Internal: Builds the cache key.
  def self.cache_key
  end

  # Holds connections.
  # @api private
  class Pool
    def checkout
    end
  end
end
